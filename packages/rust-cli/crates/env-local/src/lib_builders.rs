//! `LocalEnvironment` 的构造与组装（从 lib.rs 拆出）。

use super::*;

impl LocalEnvironment {
    pub fn new(home: Option<PathBuf>) -> Self {
        Self {
            home,
            writes: Ledger::default(),
            rewinds: rewind::Rewinds::default(),
            shells: None,
            snapshot: Default::default(),
            bash: false,
            os_version: tokio::sync::OnceCell::new(),
            web: None,
            poppler: pdf::Poppler::default(),
            additional: Vec::new(),
        }
    }

    /// `--add-dir` 的目录（docs/specs/rust-cli/cli-args.md §6）：环境事实里的附加工作目录（环境段与 nested memory
    /// 的范围）；权限判定的工作目录由入口另外交给判决链。
    #[must_use]
    pub fn with_additional_directories(mut self, dirs: Vec<EnvPath>) -> Self {
        self.additional = dirs;
        self
    }

    /// 启用 WebFetch 的网络出口（docs/specs/rust-cli/tools-more.md §4）。
    #[must_use]
    pub fn with_web(mut self, egress: std::sync::Arc<rust_cli_net::Egress>) -> Self {
        self.web = Some(egress);
        self
    }

    /// 启用 shell 命令（Bash 工具）。
    #[must_use]
    pub fn with_shell(mut self, config: ShellConfig) -> Self {
        self.snapshot = config.env.clone();
        self.bash = config.bash.is_some();
        self.shells = Some(Shells::new(config));
        self
    }

    /// git、pdfinfo 与 pdftoppm 子进程的环境：白名单投影，`SHELL` 照快照继承（F58：以前没设时写空串）。
    pub(crate) fn git_env(&self) -> std::collections::BTreeMap<String, String> {
        environ::project(&self.snapshot, None)
    }

    /// 执行环境此刻有没有“结果还留在这里”的状态（常驻执行器的空闲判断，remote-exec.md §19.5）：还在跑的命令数与
    /// 还没 ack 的后台通知数。不是端口方法：只有执行器的入口看它。
    pub fn busy(&self) -> Busy {
        self.shells
            .as_ref()
            .map_or_else(Busy::default, Shells::busy)
    }

    pub(crate) fn shells(&self) -> Result<&Shells, EnvError> {
        self.shells
            .as_ref()
            .ok_or_else(|| unsupported("shell commands"))
    }
}
