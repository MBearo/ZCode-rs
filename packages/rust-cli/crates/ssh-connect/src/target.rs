//! SSH 的目标与 ssh 的命令行（remote-exec.md §19.9）。

use std::path::PathBuf;

/// 一个 SSH 目标。
#[derive(Debug, Clone)]
pub struct SshTarget {
    /// `~/.ssh/config` 里的别名或 `user@host`。
    pub destination: String,
    /// ssh 程序（缺省 `ssh`，由组装根按 PATH 找；测试换成别的）。
    pub program: PathBuf,
    /// `-F <文件>`：测试的临时配置（不读用户的 `~/.ssh/config`）。
    pub config: Option<PathBuf>,
    /// 额外的 `-o` 选项（`Name=Value`）。
    pub options: Vec<String>,
    /// 给 ssh 进程的环境（组装根从 EnvSnapshot 给：`HOME`、`PATH`、`SSH_AUTH_SOCK` 等）。
    pub env: Vec<(String, String)>,
}

impl SshTarget {
    /// 跑 `remote_command` 的 ssh 参数：`[-F 配置] -T -o BatchMode=yes [-o …] <目标> <远端命令>`。
    pub fn args(&self, remote_command: &str) -> Result<Vec<String>, String> {
        // 以 `-` 开头的目标会被 ssh 当成选项。
        if self.destination.is_empty() || self.destination.starts_with('-') {
            return Err(format!("not an ssh destination: {:?}", self.destination));
        }
        let mut args = Vec::new();
        if let Some(config) = &self.config {
            args.push("-F".to_owned());
            args.push(config.to_string_lossy().into_owned());
        }
        args.extend(["-T", "-o", "BatchMode=yes"].map(str::to_owned));
        for option in &self.options {
            args.push("-o".to_owned());
            args.push(option.clone());
        }
        args.push(self.destination.clone());
        args.push(remote_command.to_owned());
        Ok(args)
    }

    /// 日志与错误里的名字。
    pub fn describe(&self) -> String {
        format!("ssh:{}", self.destination)
    }
}

/// POSIX shell 的单引号转义：远端的登录 shell 把 ssh 收到的命令串再解析一次。
pub fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}
