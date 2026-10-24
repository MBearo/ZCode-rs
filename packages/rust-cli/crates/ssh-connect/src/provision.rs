//! SSH 的部署器（remote-exec.md §19.9）：探测 → 选用（[`choose`]）→ 需要时上传并校验 sha256 → 给出启动执行器的命令。

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use rust_cli_base::names::UNIX_EXECUTABLE;
use rust_cli_exec_transport::{ProvisionError, Provisioned, Provisioner};

use crate::artifact::{Arch, locate, sha256};
use crate::choose::{Plan, Wanted, choose};
use crate::probe::{Probe, script};
use crate::remote;
use crate::target::{SshTarget, quote};

/// 探测的存活上限：远端一般一两秒就答完（这是存活探测，不用来同步状态）。
const PROBE_LIMIT: Duration = Duration::from_secs(60);

/// 上传脚本（POSIX sh，`sh -c '<脚本>' sh <目录> <sha256>`，stdin 是文件内容）：写同目录的临时文件，远端算 sha256，对了
/// 才改名成执行器，不对删掉报错。
pub(crate) fn upload_script() -> String {
    format!(
        r#"set -e
umask 077
dir=$1
want=$2
mkdir -p "$dir"
tmp="$dir/.{UNIX_EXECUTABLE}.upload.$$"
trap 'rm -f "$tmp"' EXIT
cat > "$tmp"
chmod 700 "$tmp"
if command -v sha256sum >/dev/null 2>&1; then got=$(sha256sum "$tmp" | cut -d' ' -f1)
elif command -v shasum >/dev/null 2>&1; then got=$(shasum -a 256 "$tmp" | cut -d' ' -f1)
elif command -v openssl >/dev/null 2>&1; then got=$(openssl dgst -sha256 -r "$tmp" | cut -d' ' -f1)
else echo "no sha256 program on the remote" >&2; exit 4
fi
if [ "$got" != "$want" ]; then echo "sha256 mismatch: got $got, want $want" >&2; exit 3; fi
mv -f "$tmp" "$dir/{UNIX_EXECUTABLE}"
trap - EXIT
echo ok
"#
    )
}

pub struct SshProvisioner {
    target: SshTarget,
    wanted: Wanted,
    /// 本机可以上传的 Linux 产物的候选文件；架构从 ELF 头读（§19.4 的 `dist/` 与测试变量给的文件）。
    artifacts: Vec<PathBuf>,
}

impl SshProvisioner {
    pub fn new(target: SshTarget, wanted: Wanted, artifacts: Vec<PathBuf>) -> Self {
        Self {
            target,
            wanted,
            artifacts,
        }
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

    async fn upload(&self, probe: &Probe, arch: Arch) -> Result<String, ProvisionError> {
        let artifact =
            locate(&self.artifacts, arch)
                .await
                .ok_or_else(|| ProvisionError::NoArtifact {
                    version: self.wanted.version.clone(),
                    target: arch.triple().to_owned(),
                })?;
        let digest = sha256(&artifact)
            .await
            .map_err(|error| ProvisionError::Upload(format!("{}: {error}", artifact.display())))?;
        let path = probe.binary_path(&self.wanted.version);
        let dir = path
            .rsplit_once('/')
            .map_or(path.as_str(), |(dir, _)| dir)
            .to_owned();
        let command = format!(
            "sh -c {} sh {} {}",
            quote(&upload_script()),
            quote(&dir),
            quote(&digest)
        );
        let file = rust_cli_fs::open_read(&artifact)
            .await
            .map_err(|error| ProvisionError::Upload(format!("{}: {error}", artifact.display())))?;
        tracing::info!(target: "ssh_connect", target_host = %self.target.destination, artifact = %artifact.display(), remote = %path, "uploading the executor");
        let output = remote::run(&self.target, &command, Box::new(file), None)
            .await
            .map_err(|message| self.unreachable(message))?;
        if !output.success() || output.stdout.trim() != "ok" {
            return Err(ProvisionError::Upload(output.tail()));
        }
        Ok(path)
    }
}

#[async_trait]
impl Provisioner for SshProvisioner {
    async fn provision(&self) -> Result<Provisioned, ProvisionError> {
        let probe = self.probe().await?;
        let artifact = match Arch::of(&probe.os, &probe.arch) {
            Some(arch) => locate(&self.artifacts, arch).await.is_some(),
            None => false,
        };
        let program = match choose(&probe, &self.wanted, artifact)? {
            Plan::Use(path) => path,
            Plan::Upload(arch) => self.upload(&probe, arch).await?,
        };
        tracing::info!(target: "ssh_connect", target_host = %self.target.destination, %program, "executor provisioned");
        Ok(Provisioned {
            program,
            args: vec!["executor".to_owned(), "--stdio".to_owned()],
        })
    }
}
