//! 部署配置（docs/specs/rust-cli/deployment.md §6）：会话的主 env 在哪。缺省全本地，与没有这份配置时完全相同。
//!
//! 来源是环境变量 [`VAR`] 给的 JSON 文件；没设就是缺省。未知字段报错（`deny_unknown_fields`），拼错的键不会被悄悄
//! 忽略。密钥不写进这份文件：要用时写成 `*_env`（环境变量的名字）。角色不在这里，由子命令决定（deployment.md §14）。
//! 会话库只有 SQLite（云上也是）：数据库适配归存储线，`store` 键随适配器一起加；在那之前写了 `store` 按未知键报错。

use std::fmt;
use std::path::Path;

use rust_cli_base::path::DRIVE_PREFIX_LEN as DRIVE_LEN;
use serde::Deserialize;

/// 部署配置文件的路径。
pub(crate) const VAR: &str = rust_cli_base::names::env::ZCODE_DEPLOYMENT;

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Deployment {
    #[serde(default)]
    pub primary_env: PrimaryEnv,
}

/// 会话的主 env：工具不写 `machine` 时在它上面执行，规则、记忆、skills、git 都从它读（deployment.md §8）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum PrimaryEnv {
    /// 本进程里的 env-local（缺省）。
    #[default]
    Local,
    /// 经 SSH 连到的执行器（远端执行 R2 的部署与连接）。
    Ssh(SshPrimary),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SshPrimary {
    /// ssh 的目标（`user@host`，或 ssh 配置里的 Host）。
    pub destination: String,
    /// 那台机器上的工作目录：会话的工作区根。
    pub workdir: String,
    /// 给 ssh 的 `-F <文件>`。
    #[serde(default)]
    pub ssh_config: Option<String>,
    /// 上传用的执行器产物；没有时按本机版本从 npm 取（F100）。
    #[serde(default)]
    pub artifact: Option<String>,
}

/// 读不到、写错或不合规则的部署配置：入口照原文报给用户，退出码 1。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeploymentError {
    /// 配置文件的路径；缺省配置不会出错，所以总有。
    pub path: String,
    pub reason: String,
}

impl fmt::Display for DeploymentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{VAR}={}: {}", self.path, self.reason)
    }
}

impl std::error::Error for DeploymentError {}

impl Deployment {
    /// 解析并校验一份配置的原文。
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let deployment: Self = serde_json::from_str(text).map_err(|error| error.to_string())?;
        deployment.validate()?;
        Ok(deployment)
    }

    fn validate(&self) -> Result<(), String> {
        if let PrimaryEnv::Ssh(ssh) = &self.primary_env {
            if ssh.destination.trim().is_empty() {
                return Err("primary_env.ssh.destination is empty".to_owned());
            }
            if !is_absolute(&ssh.workdir) {
                return Err(format!(
                    "primary_env.ssh.workdir must be an absolute path on that machine, got {:?}",
                    ssh.workdir
                ));
            }
        }
        Ok(())
    }

    /// 主 env 就在本进程里。
    pub(crate) fn primary_is_local(&self) -> bool {
        matches!(self.primary_env, PrimaryEnv::Local)
    }
}

/// 远端的路径风格要连上之后才知道，这里两种绝对路径都认：`/…` 与 `C:\…`、`C:/…`。
fn is_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with('/')
        || (bytes.len() > DRIVE_LEN
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[DRIVE_LEN], b'\\' | b'/'))
}

/// 从入口的环境快照读部署配置：没设或为空时是缺省（全本地）。
///
/// 修复（F129，审查 E8）：以前在快照里按原样比较名字，绕过了 Windows 上不分大小写的规则（F58）；现在经 `text`。
pub(crate) async fn load(
    env: &rust_cli_base::secret::EnvSnapshot,
) -> Result<Deployment, DeploymentError> {
    let Some(path) = env.text(VAR) else {
        return Ok(Deployment::default());
    };
    let error = |reason: String| DeploymentError {
        path: path.to_owned(),
        reason,
    };
    let bytes = rust_cli_fs::read_optional(Path::new(path))
        .await
        .map_err(|io| error(io.to_string()))?
        .ok_or_else(|| error("no such file".to_owned()))?;
    let text = String::from_utf8(bytes).map_err(|_| error("not UTF-8".to_owned()))?;
    Deployment::parse(&text).map_err(error)
}

#[cfg(test)]
#[path = "deployment_tests.rs"]
mod tests;
