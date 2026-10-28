//! SSH 的部署器（remote-exec.md §19.9）：探测 → 选用（[`choose`]）→ 需要时上传并校验 sha256 → 给出启动执行器的命令。
//!
//! F100（§26.2）：要上传而本机没有远端平台的静态产物时，经注入的 [`ArtifactSource`] 拿产物（本机缓存，没有就下载）；
//! 拿不到时退到协议相同的候选。F103（§27.2）：本机的版本不在注册表里（本地 release 编译）、也没有协议相同的候选时，
//! 再用注册表的 latest，改名前试跑核对协议。F107（§28）：平台是“系统 + 架构”，macOS arm64 也走这条路；Intel Mac 没有
//! 可下载的包（报 `NoPackage`），试跑被系统杀掉时报 `Unsigned`。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rust_cli_base::names::UNIX_EXECUTABLE;
use rust_cli_exec_transport::{ProvisionError, ProvisionStep, Provisioned, Provisioner};

use crate::artifact::{Platform, locate, sha256};
use crate::choose::{Plan, Wanted, choose};
use crate::probe::{Libc, Probe, Report, script};
use crate::remote::{self, Output};
use crate::source::{ArtifactSource, DownloadError};
use crate::target::{SshTarget, quote};

/// 探测的存活上限：远端一般一两秒就答完（这是存活探测，不用来同步状态）。
const PROBE_LIMIT: Duration = Duration::from_secs(60);

/// 试跑失败时上传脚本的退出码（F100，§26.5）。
const TRIAL_FAILED: i32 = 5;
/// 试跑出来的协议主版本不对时上传脚本的退出码（F103，§27.5）。
const PROTOCOL_MISMATCH: i32 = 6;
/// 试跑被 SIGKILL 杀掉时 shell 看到的退出码（F107，§28.5：macOS 上签名无效就是这样）。
const KILLED: i32 = 137;

/// 上传脚本（POSIX sh，`sh -c '<脚本>' sh <目录> <sha256> [check [<协议>]]`，stdin 是文件内容）：写同目录的临时文件，
/// 远端算 sha256，对了才改名成执行器，不对删掉报错。第三个参数是 `check` 时改名之前先试跑一次（F100：下载的 glibc
/// 产物，远端的 glibc 太旧时跑不起来）；退出码不是 0（在跑）或 3（没在跑）时删掉、带回 stderr、以 5 退出。再给了第四个
/// 参数（F103：npm 的 latest）时，试跑输出的第一行里第一个 `"protocol":` 的数字（`status --json` 的键按 version、
/// protocol 的顺序写出，第一个就是这个二进制自己的，§19.6）要等于它；不等时删掉、把这一行写到 stderr、以 6 退出。
pub(crate) fn upload_script() -> String {
    format!(
        r#"set -e
umask 077
dir=$1
want=$2
mkdir -p "$dir"
tmp="$dir/.{UNIX_EXECUTABLE}.upload.$$"
trap 'rm -f "$tmp" "$tmp.err" "$tmp.out"' EXIT
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
  "$tmp" executor status --json </dev/null >"$tmp.out" 2>"$tmp.err" || code=$?
  if [ "$code" != 0 ] && [ "$code" != 3 ]; then
    cat "$tmp.err" >&2
    echo "the executor did not run on the remote (exit $code)" >&2
    exit {TRIAL_FAILED}
  fi
  if [ -n "${{4:-}}" ]; then
    line=
    IFS= read -r line <"$tmp.out" || :
    rest=${{line#*'"protocol":'}}
    if [ "$rest" = "$line" ] || [ "${{rest%%[!0-9]*}}" != "$4" ]; then
      printf '%s\n' "$line" >&2
      exit {PROTOCOL_MISMATCH}
    fi
  fi
fi
mv -f "$tmp" "$dir/{UNIX_EXECUTABLE}"
trap - EXIT
rm -f "$tmp.err" "$tmp.out"
echo ok
"#
    )
}

/// 上传之后、改名之前的试跑（§26.5、§27.5）。
#[derive(Debug, Clone, Copy)]
enum Trial {
    /// 不试跑（本机的静态产物：不依赖远端的 libc）。
    Skip,
    /// 试跑，跑得起来就行（下载的同版本产物）。
    Run,
    /// 试跑并核对协议主版本（注册表的 latest，F103）；平台给错误里的本机缓存路径。
    Protocol(Platform),
}

pub struct SshProvisioner {
    target: SshTarget,
    wanted: Wanted,
    /// 本机可以上传的静态产物的候选文件；平台从产物的头读（§19.4 的 `dist/` 的 musl 目录与测试变量给的文件，§28.4）。
    artifacts: Vec<PathBuf>,
    /// 没有静态产物时拿产物的来源（F100；None 时不下载：debug 构建没有给假注册表时、R2 的测试入口）。
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

    /// 加上下载产物的来源（F100，§26.7）：同一个进程的所有机器共用一个（单飞靠它）。
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

    /// 上传 `artifact` 到远端的 `bin/<version>/`（按产物的实际版本，F103：npm 的 latest 不放进本机版本的目录）；
    /// `trial`：改名之前怎么试跑（§26.5、§27.5）。
    async fn upload(
        &self,
        probe: &Probe,
        (artifact, version): (&Path, &str),
        trial: Trial,
        progress: &(dyn Fn(ProvisionStep) + Send + Sync),
    ) -> Result<String, ProvisionError> {
        let digest = sha256(artifact)
            .await
            .map_err(|error| ProvisionError::Upload(format!("{}: {error}", artifact.display())))?;
        let path = probe.binary_path(version);
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
        match trial {
            Trial::Skip => {}
            Trial::Run => command.push_str(" check"),
            Trial::Protocol(_) => command.push_str(&format!(" check {}", self.wanted.protocol)),
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
            return Err(self.upload_failed(probe, &output, version, trial));
        }
        Ok(path)
    }

    /// 上传失败的原因：latest 试跑出来协议不对时写明两边的版本与协议（F103，§27.5）；其余同 [`upload_failed`]。
    fn upload_failed(
        &self,
        probe: &Probe,
        output: &Output,
        version: &str,
        trial: Trial,
    ) -> ProvisionError {
        if output.status == Some(PROTOCOL_MISMATCH)
            && let Trial::Protocol(platform) = trial
            && let Some(report) = output
                .stderr
                .lines()
                .rev()
                .find_map(|line| serde_json::from_str::<Report>(line.trim()).ok())
        {
            return ProvisionError::LatestIncompatible {
                version: self.wanted.version.clone(),
                protocol: self.wanted.protocol,
                latest: version.to_owned(),
                latest_protocol: report.protocol,
                target: platform.triple().to_owned(),
            };
        }
        upload_failed(probe, output)
    }

    /// 本机缓存里 `version` 这个平台的产物；没有就下载。`reported`：“下载执行器”只报一次（F103 退到 latest 时不再报）。
    /// 来源在写进缓存之前已经核对过产物的头；拿错平台的文件时上传后的试跑也会失败。
    async fn fetch(
        &self,
        source: &dyn ArtifactSource,
        (version, platform): (&str, Platform),
        reported: &mut bool,
        progress: &(dyn Fn(ProvisionStep) + Send + Sync),
    ) -> Result<PathBuf, DownloadError> {
        if let Some(file) = source.cached(version, platform).await {
            return Ok(file);
        }
        if !std::mem::replace(reported, true) {
            progress(ProvisionStep::Downloading);
        }
        source.download(version, platform).await
    }

    /// 本机没有静态产物时经来源拿产物（§26.2）：同版本的（缓存或下载）→ 协议相同的候选 → 本机的版本不在注册表里时
    /// 用 latest（F103，§27.2）→ 报错。
    async fn through_source(
        &self,
        probe: &Probe,
        source: &dyn ArtifactSource,
        platform: Platform,
        progress: &(dyn Fn(ProvisionStep) + Send + Sync),
    ) -> Result<String, ProvisionError> {
        let version = self.wanted.version.as_str();
        let mut reported = false;
        let error = match self
            .fetch(source, (version, platform), &mut reported, progress)
            .await
        {
            Ok(file) => {
                return self
                    .upload(probe, (&file, version), Trial::Run, progress)
                    .await;
            }
            Err(error) => error,
        };
        tracing::warn!(target: "ssh_connect", target_host = %self.target.destination, reason = %error.reason(), "the executor could not be downloaded");
        // 下载失败之后先用协议相同的候选；`refused`：没有 latest 可退时报的错（空闲而协议不同的常驻进程，F100 照旧）。
        let refused = match choose(probe, &self.wanted, false) {
            Ok(Plan::Use(path)) => return Ok(path),
            Ok(Plan::Upload(_)) | Err(ProvisionError::NoArtifact { .. }) => None,
            Err(error @ ProvisionError::Incompatible { busy: false, .. }) => Some(error),
            Err(other) => return Err(other),
        };
        match error {
            // 只有“这个版本不在注册表里”才退到 latest；网络错误、校验不对不退（F103，§27.3）。
            DownloadError::Unpublished {
                latest: Some(latest),
                ..
            } if latest != version => {
                self.latest(probe, source, (&latest, platform), &mut reported, progress)
                    .await
            }
            error => Err(refused.unwrap_or_else(|| ProvisionError::DownloadFailed {
                version: version.to_owned(),
                target: platform.triple().to_owned(),
                reason: error.reason().to_owned(),
            })),
        }
    }

    /// 退到注册表的 latest（F103，§27.2）：本机缓存里有就不下载；上传到 `bin/<latest>/`，改名前试跑核对协议。
    async fn latest(
        &self,
        probe: &Probe,
        source: &dyn ArtifactSource,
        (latest, platform): (&str, Platform),
        reported: &mut bool,
        progress: &(dyn Fn(ProvisionStep) + Send + Sync),
    ) -> Result<String, ProvisionError> {
        let version = &self.wanted.version;
        tracing::info!(target: "ssh_connect", target_host = %self.target.destination, local = %version, %latest, "this zcode version is not on npm; using the latest published executor");
        let file = self
            .fetch(source, (latest, platform), reported, progress)
            .await
            .map_err(|error| ProvisionError::DownloadFailed {
                version: version.clone(),
                target: platform.triple().to_owned(),
                reason: format!(
                    "zcode {version} is not on npm, and the latest, {latest}, could not be downloaded: {}",
                    error.reason()
                ),
            })?;
        self.upload(probe, (&file, latest), Trial::Protocol(platform), progress)
            .await
    }
}

/// 上传失败的原因：试跑时加载器报缺 `GLIBC_x.y` 是远端的 glibc 太旧（§26.5），写明远端的版本；macOS 的远端上试跑被
/// SIGKILL 杀掉是签名无效（F107，§28.5）；其余照旧。
fn upload_failed(probe: &Probe, output: &Output) -> ProvisionError {
    if output.status == Some(TRIAL_FAILED) {
        if let Some(needs) = missing_glibc(&output.stderr) {
            return ProvisionError::OldGlibc {
                glibc: match &probe.libc {
                    Libc::Glibc(version) => Some(version.clone()),
                    Libc::Unknown | Libc::Musl => None,
                },
                needs,
            };
        }
        // 缺陷修复（F107）：macOS 认为签名无效时不是“跑起来又失败”，而是 SIGKILL（shell 看到 137），stderr 里多半
        // 只有 `Killed: 9`。只在 macOS 的远端上这么判：Linux 上的 137 是 OOM 之类，与签名无关。
        if probe.platform().is_some_and(Platform::is_macos) && killed(&output.stderr) {
            return ProvisionError::Unsigned;
        }
    }
    ProvisionError::Upload(output.tail())
}

/// 试跑的退出码是 SIGKILL 吗（上传脚本把它写成 `(exit 137)`，F107，§28.5）。
fn killed(stderr: &str) -> bool {
    stderr.contains(&format!("(exit {KILLED})"))
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
        let platform = probe.platform();
        let local = match platform {
            Some(platform) => locate(&self.artifacts, platform).await,
            None => None,
        };
        // 下载来源用不上的两种远端（远端的候选与本机的静态产物不受影响）：musl 的 Linux（§26.5），与没有发布平台包的
        // 平台（F107，§28.2：Intel Mac）。
        let source = self
            .source
            .as_deref()
            .filter(|_| probe.libc != Libc::Musl)
            .filter(|_| platform.is_none_or(Platform::published));
        let plan =
            choose(&probe, &self.wanted, local.is_some() || source.is_some()).map_err(|error| {
                match error {
                    ProvisionError::NoArtifact { .. }
                        if self.source.is_some() && source.is_none() =>
                    {
                        match platform.filter(|platform| !platform.published()) {
                            Some(platform) => ProvisionError::NoPackage {
                                package: platform.package_id().to_owned(),
                            },
                            None => ProvisionError::Musl,
                        }
                    }
                    other => other,
                }
            })?;
        let program = match (plan, local, source) {
            (Plan::Use(path), _, _) => path,
            (Plan::Upload(_), Some(file), _) => {
                self.upload(&probe, (&file, &self.wanted.version), Trial::Skip, progress)
                    .await?
            }
            (Plan::Upload(platform), None, Some(source)) => {
                self.through_source(&probe, source, platform, progress)
                    .await?
            }
            // choose 只在可以上传时给 Upload。
            (Plan::Upload(platform), None, None) => {
                return Err(ProvisionError::NoArtifact {
                    version: self.wanted.version.clone(),
                    target: platform.missing_triple().to_owned(),
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
