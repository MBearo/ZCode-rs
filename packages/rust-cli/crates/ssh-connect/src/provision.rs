//! SSH 的部署器（remote-exec.md §19.9）：探测 → 选用（[`choose`]）→ 需要时上传并校验 sha256 → 给出启动执行器的命令。
//!
//! F100（§26.2）：要上传而本机没有远端架构的静态产物时，经注入的 [`ArtifactSource`] 拿 glibc 的产物（本机缓存，没有就
//! 下载）；拿不到时退到协议相同的候选。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rust_cli_base::names::UNIX_EXECUTABLE;
use rust_cli_exec_transport::{ProvisionError, ProvisionStep, Provisioned, Provisioner};

use crate::artifact::{Arch, locate, sha256};
use crate::choose::{Plan, Wanted, choose};
use crate::probe::{Libc, Probe, script};
use crate::remote::{self, Output};
use crate::source::ArtifactSource;
use crate::target::{SshTarget, quote};

/// 探测的存活上限：远端一般一两秒就答完（这是存活探测，不用来同步状态）。
const PROBE_LIMIT: Duration = Duration::from_secs(60);

/// 试跑失败时上传脚本的退出码（F100，§26.5）。
const TRIAL_FAILED: i32 = 5;

/// 上传脚本（POSIX sh，`sh -c '<脚本>' sh <目录> <sha256> [check]`，stdin 是文件内容）：写同目录的临时文件，远端算
/// sha256，对了才改名成执行器，不对删掉报错。第三个参数是 `check` 时改名之前先试跑一次（F100：下载的 glibc 产物，远端的
/// glibc 太旧时跑不起来）；退出码不是 0（在跑）或 3（没在跑）时删掉、带回 stderr、以 5 退出。
pub(crate) fn upload_script() -> String {
    format!(
        r#"set -e
umask 077
dir=$1
want=$2
mkdir -p "$dir"
tmp="$dir/.{UNIX_EXECUTABLE}.upload.$$"
trap 'rm -f "$tmp" "$tmp.err"' EXIT
cat > "$tmp"
chmod 700 "$tmp"
if command -v sha256sum >/dev/null 2>&1; then got=$(sha256sum "$tmp" | cut -d' ' -f1)
elif command -v shasum >/dev/null 2>&1; then got=$(shasum -a 256 "$tmp" | cut -d' ' -f1)
elif command -v openssl >/dev/null 2>&1; then got=$(openssl dgst -sha256 -r "$tmp" | cut -d' ' -f1)
else echo "no sha256 program on the remote" >&2; exit 4
fi
if [ "$got" != "$want" ]; then echo "sha256 mismatch: got $got, want $want" >&2; exit 3; fi
if [ "${{3:-}}" = check ]; then
  code=0
  "$tmp" executor status --json </dev/null >/dev/null 2>"$tmp.err" || code=$?
  if [ "$code" != 0 ] && [ "$code" != 3 ]; then
    cat "$tmp.err" >&2
    echo "the executor did not run on the remote (exit $code)" >&2
    exit {TRIAL_FAILED}
  fi
fi
mv -f "$tmp" "$dir/{UNIX_EXECUTABLE}"
trap - EXIT
rm -f "$tmp.err"
echo ok
"#
    )
}

pub struct SshProvisioner {
    target: SshTarget,
    wanted: Wanted,
    /// 本机可以上传的静态 Linux 产物的候选文件；架构从 ELF 头读（§19.4 的 `dist/` 的 musl 目录与测试变量给的文件）。
    artifacts: Vec<PathBuf>,
    /// 没有静态产物时拿 glibc 产物的来源（F100；None 时不下载：debug 构建没有给假注册表时、R2 的测试入口）。
    source: Option<Arc<dyn ArtifactSource>>,
}

impl SshProvisioner {
    pub fn new(target: SshTarget, wanted: Wanted, artifacts: Vec<PathBuf>) -> Self {
        Self {
            target,
            wanted,
            artifacts,
            source: None,
        }
    }

    /// 加上 glibc 产物的来源（F100，§26.7）：同一个进程的所有机器共用一个（单飞靠它）。
    pub fn with_source(mut self, source: Arc<dyn ArtifactSource>) -> Self {
        self.source = Some(source);
        self
    }

    fn unreachable(&self, message: String) -> ProvisionError {
        ProvisionError::Unreachable {
            target: self.target.destination.clone(),
            message,
        }
    }

    /// 探测远端。
    pub async fn probe(&self) -> Result<Probe, ProvisionError> {
        let input = Box::new(std::io::Cursor::new(script().into_bytes()));
        let output = remote::run(&self.target, "sh -s", input, Some(PROBE_LIMIT))
            .await
            .map_err(|message| self.unreachable(message))?;
        if !output.success() {
            return Err(self.unreachable(output.tail()));
        }
        Probe::parse(&output.stdout).map_err(ProvisionError::Probe)
    }

    /// 上传 `artifact`；`check`：改名之前在远端试跑（下载的 glibc 产物，§26.5）。
    async fn upload(
        &self,
        probe: &Probe,
        artifact: &Path,
        check: bool,
        progress: &(dyn Fn(ProvisionStep) + Send + Sync),
    ) -> Result<String, ProvisionError> {
        let digest = sha256(artifact)
            .await
            .map_err(|error| ProvisionError::Upload(format!("{}: {error}", artifact.display())))?;
        let path = probe.binary_path(&self.wanted.version);
        let dir = path
            .rsplit_once('/')
            .map_or(path.as_str(), |(dir, _)| dir)
            .to_owned();
        let mut command = format!(
            "sh -c {} sh {} {}",
            quote(&upload_script()),
            quote(&dir),
            quote(&digest)
        );
        if check {
            command.push_str(" check");
        }
        let file = rust_cli_fs::open_read(artifact)
            .await
            .map_err(|error| ProvisionError::Upload(format!("{}: {error}", artifact.display())))?;
        // 上传之前报大小（F98，remote-exec.md §25.4.2：状态行写“上传执行器（N MB）”）。
        let bytes = rust_cli_fs::file_len(artifact)
            .await
            .ok()
            .flatten()
            .unwrap_or(0);
        progress(ProvisionStep::Uploading { bytes });
        tracing::info!(target: "ssh_connect", target_host = %self.target.destination, artifact = %artifact.display(), remote = %path, "uploading the executor");
        let output = remote::run(&self.target, &command, Box::new(file), None)
            .await
            .map_err(|message| self.unreachable(message))?;
        if !output.success() || output.stdout.trim() != "ok" {
            return Err(upload_failed(probe, &output));
        }
        Ok(path)
    }

    /// 本机缓存里的 glibc 产物；没有就下载（报 Downloading）。来源在写进缓存之前已经核对过 ELF 头；拿错架构的文件时
    /// 上传后的试跑也会失败。
    async fn fetch(
        &self,
        source: &dyn ArtifactSource,
        arch: Arch,
        progress: &(dyn Fn(ProvisionStep) + Send + Sync),
    ) -> Result<PathBuf, String> {
        let version = &self.wanted.version;
        match source.cached(version, arch).await {
            Some(file) => Ok(file),
            None => {
                progress(ProvisionStep::Downloading);
                source.download(version, arch).await
            }
        }
    }

    /// 下载失败之后（§26.2）：协议相同的候选；没有时报 `DownloadFailed`，其他错误照旧。
    fn without_download(
        &self,
        probe: &Probe,
        arch: Arch,
        reason: String,
    ) -> Result<String, ProvisionError> {
        tracing::warn!(target: "ssh_connect", target_host = %self.target.destination, %reason, "the executor could not be downloaded");
        match choose(probe, &self.wanted, false) {
            Ok(Plan::Use(path)) => Ok(path),
            Ok(Plan::Upload(_)) | Err(ProvisionError::NoArtifact { .. }) => {
                Err(ProvisionError::DownloadFailed {
                    version: self.wanted.version.clone(),
                    target: arch.gnu_triple().to_owned(),
                    reason,
                })
            }
            Err(other) => Err(other),
        }
    }
}

/// 上传失败的原因：试跑时加载器报缺 `GLIBC_x.y` 是远端的 glibc 太旧（§26.5），写明远端的版本；其余照旧。
fn upload_failed(probe: &Probe, output: &Output) -> ProvisionError {
    if output.status == Some(TRIAL_FAILED)
        && let Some(needs) = missing_glibc(&output.stderr)
    {
        return ProvisionError::OldGlibc {
            glibc: match &probe.libc {
                Libc::Glibc(version) => Some(version.clone()),
                Libc::Unknown | Libc::Musl => None,
            },
            needs,
        };
    }
    ProvisionError::Upload(output.tail())
}

/// 加载器报缺的最高的 `GLIBC_x.y`（`version 'GLIBC_2.34' not found`）；没有时 None。
pub(crate) fn missing_glibc(stderr: &str) -> Option<String> {
    stderr
        .lines()
        .filter(|line| line.contains("not found"))
        .flat_map(|line| {
            line.match_indices("GLIBC_")
                .map(move |(at, _)| &line[at + 6..])
        })
        .filter_map(|rest| {
            let version: String = rest
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            let parts: Vec<u32> = version
                .split('.')
                .map(str::parse)
                .collect::<Result<_, _>>()
                .ok()?;
            Some((parts, version))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, version)| format!("GLIBC_{version}"))
}

#[async_trait]
impl Provisioner for SshProvisioner {
    async fn provision(&self) -> Result<Provisioned, ProvisionError> {
        self.provision_reporting(&|_| {}).await
    }

    async fn provision_reporting(
        &self,
        progress: &(dyn Fn(ProvisionStep) + Send + Sync),
    ) -> Result<Provisioned, ProvisionError> {
        let probe = self.probe().await?;
        let local = match Arch::of(&probe.os, &probe.arch) {
            Some(arch) => locate(&self.artifacts, arch).await,
            None => None,
        };
        // 下载的包要 glibc：远端是 musl 时不拿（远端的候选与本机的静态产物不受影响，§26.5）。
        let source = self.source.as_deref().filter(|_| probe.libc != Libc::Musl);
        let plan =
            choose(&probe, &self.wanted, local.is_some() || source.is_some()).map_err(|error| {
                match error {
                    ProvisionError::NoArtifact { .. }
                        if self.source.is_some() && source.is_none() =>
                    {
                        ProvisionError::Musl
                    }
                    other => other,
                }
            })?;
        let program = match (plan, local, source) {
            (Plan::Use(path), _, _) => path,
            (Plan::Upload(_), Some(file), _) => self.upload(&probe, &file, false, progress).await?,
            (Plan::Upload(arch), None, Some(source)) => {
                match self.fetch(source, arch, progress).await {
                    Ok(file) => self.upload(&probe, &file, true, progress).await?,
                    Err(reason) => self.without_download(&probe, arch, reason)?,
                }
            }
            // choose 只在可以上传时给 Upload。
            (Plan::Upload(arch), None, None) => {
                return Err(ProvisionError::NoArtifact {
                    version: self.wanted.version.clone(),
                    target: arch.musl_triple().to_owned(),
                });
            }
        };
        tracing::info!(target: "ssh_connect", target_host = %self.target.destination, %program, "executor provisioned");
        Ok(Provisioned {
            program,
            args: vec!["executor".to_owned(), "--stdio".to_owned()],
        })
    }
}
