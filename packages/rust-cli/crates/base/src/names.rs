//! 产品目录与文件的名字：同一个名字只定义一处（docs/specs/rust-cli/README.md §4.1，工作线 F55）。
//!
//! 名字放在 base：内核 crate（context、tools、agents、ext、policy、kernel-api）与 prompts 不能依赖 paths
//! （README §3.1 规则 1），而所有 crate 都能依赖 base。本机存储里用户根 `~/.zcode` 的位置只在 paths 的
//! `user_path` 计算；执行环境里的路径（项目的 `<目录>/.zcode`、执行环境 home 下的 `~/.zcode`、managed 目录）
//! 由各能力按执行环境的路径写法拼，名字都取这里。
//!
//! 边界脚本（`scripts/rust-cli/boundaries`，`SINGLE_DEFINITIONS`）拒绝本文件以外的产品代码在字符串字面量里
//! 写 `.zcode` 路径段。

/// 产品目录名 `.zcode`，给 `concat!` 拼 `const` 用（`const` 里不能用 [`DOT_DIR`]）；别处写 [`DOT_DIR`]。
#[macro_export]
macro_rules! dot_dir {
    () => {
        ".zcode"
    };
}

/// 产品目录名：用户根 `~/.zcode`、项目的 `<目录>/.zcode`、managed 目录下的 `.zcode`。
pub const DOT_DIR: &str = dot_dir!();

/// 规则文件。
pub const RULES_FILE: &str = "AGENTS.md";

/// 本地私有的规则文件。
pub const LOCAL_RULES_FILE: &str = "AGENTS.local.md";

/// 项目根与各层目录下的项目配置（Node 的 `zcode.json`，带 MCP 服务器、hooks 与权限）：入口读它（cli
/// `config/files.rs`），写入它要确认（policy 的敏感文件，F58）。
pub const PROJECT_CONFIG_FILE: &str = "zcode.json";

/// 项目里 `.zcode/` 下的 worktree 目录：EnterWorktree 在这里建（env-local），权限判定认它（tools）。
pub const WORKTREES_DIR: &str = "worktrees";

/// 项目里 `.zcode/` 下的自定义子代理目录：子代理从这里加载（agents），sandbox 禁止写它（env-local）。
pub const AGENTS_DIR: &str = "agents";

/// 项目里 `.zcode/` 下的定时任务文件：env-local 读写，工具的结果文字提到它。
pub const SCHEDULED_TASKS_FILE: &str = "scheduled_tasks.json";

/// Node 的官方插件市场名（`ZCODE_OFFICIAL_PLUGIN_MARKETPLACE`）：插件 id 的后半、缓存与分片的目录名。platform 的
/// 已安装清单、ext 的插件选择、入口合并 `plugins` 段都用它（F55：以前三处各写一份）。
pub const OFFICIAL_MARKETPLACE: &str = "zcode-plugins-official";

/// 执行器的目录（docs/specs/rust-cli/remote-exec.md §19.4）：用户根下的 `rust-cli/exec`。本机的位置由 paths 的
/// `StoragePaths::executor()` 算；远端的位置由 ssh-connect 按远端的 home 拼（远端只用这个目录，不碰 `cli/`）。
pub const EXECUTOR_HOME: &str = "rust-cli/exec";

/// Unix 上的可执行文件名（cli-args.md §13）：上传到远端的执行器、远端探测的候选都是这个名字。
pub const UNIX_EXECUTABLE: &str = "zcode-rs";

/// 发给服务的客户端名：MCP `initialize` 的 `clientInfo.name` 与签发项目令牌的 `clientType`（F55：以前 ext 与
/// platform 各写一份；版本号由入口传下去）。
pub const CLIENT_NAME: &str = "zcode";

/// 给用户程序的变量名（hook 进程的环境、插件 MCP 的环境与 `${…}` 配置变量、插件 agent 与 skill 正文里的 `${…}`）：
/// Node 两套都给，`ZCODE_*` 与另一种插件格式用的名字（同时兼容两种插件，config.md §2.5.1）。F55：以前 hooks、ext、
/// agents 各写一份；各处的顺序与集合不同（例如 skill 正文不认另一种格式的第二个会话 ID 名），没有统一，只共用名字。
pub mod vars {
    pub const ZCODE_PLUGIN_ROOT: &str = "ZCODE_PLUGIN_ROOT";
    pub const CLAUDE_PLUGIN_ROOT: &str = "CLAUDE_PLUGIN_ROOT";
    pub const ZCODE_PROJECT_DIR: &str = "ZCODE_PROJECT_DIR";
    pub const CLAUDE_PROJECT_DIR: &str = "CLAUDE_PROJECT_DIR";
    pub const ZCODE_SESSION_ID: &str = "ZCODE_SESSION_ID";
    pub const CLAUDE_SESSION_ID: &str = "CLAUDE_SESSION_ID";
    pub const CLAUDE_CODE_SESSION_ID: &str = "CLAUDE_CODE_SESSION_ID";
    pub const ZCODE_SKILL_DIR: &str = "ZCODE_SKILL_DIR";
    pub const CLAUDE_SKILL_DIR: &str = "CLAUDE_SKILL_DIR";

    /// 文本里的引用写法 `${NAME}`。
    pub fn placeholder(name: &str) -> String {
        format!("${{{name}}}")
    }
}

/// 构建时由 build.rs 给出的版本号的变量名：`env!` 只收字面量，给它用宏（`env!(rust_cli_base::zcode_build_version_var!())`）。
#[macro_export]
macro_rules! zcode_build_version_var {
    () => {
        "ZCODE_BUILD_VERSION"
    };
}

/// 业务代码读写的环境变量名（工作线 F121，README §4.1 第 6 条）：同一个名字只在这里写一次。边界脚本
/// （`scripts/rust-cli/boundaries/literals.mjs` 的 `env var name`）拒绝本文件以外的产品代码在字符串字面量里写环境变量名。
/// 只有入口读进程环境（README §4 第一层）；别的 crate 拿入口给的快照或给子进程设变量时同样用这里的名字。
#[expect(
    non_upper_case_globals,
    reason = "常量名就是变量名本身（Windows 的变量名大小写混写）"
)]
pub mod env {
    /// 构建时给出的版本号（[`crate::zcode_build_version_var`]）。
    pub const ZCODE_BUILD_VERSION: &str = crate::zcode_build_version_var!();

    // 本工程的入口读的变量：存储、配置、显式覆盖的端点（config.md §2.5）。
    pub const ZCODE_DATA_BASE_DIR: &str = "ZCODE_DATA_BASE_DIR";
    pub const ZCODE_BUILTIN_PROVIDER_CONFIG_FILE: &str = "ZCODE_BUILTIN_PROVIDER_CONFIG_FILE";
    pub const ZCODE_CREDENTIAL_SECRET: &str = "ZCODE_CREDENTIAL_SECRET";
    pub const ZCODE_MODEL_BASE_URL: &str = "ZCODE_MODEL_BASE_URL";
    pub const ZCODE_API_KEY: &str = "ZCODE_API_KEY";
    pub const ZCODE_MODEL: &str = "ZCODE_MODEL";
    pub const ZCODE_STORAGE_DIR: &str = "ZCODE_STORAGE_DIR";
    pub const ZCODE_SESSION_DB_PATH: &str = "ZCODE_SESSION_DB_PATH";
    pub const ZCODE_SESSION_DB: &str = "ZCODE_SESSION_DB";
    pub const ZCODE_RUNTIME_ENV: &str = "ZCODE_RUNTIME_ENV";
    pub const ZCODE_APP_VERSION: &str = "ZCODE_APP_VERSION";
    pub const ZCODE_BUBBLEWRAP: &str = "ZCODE_BUBBLEWRAP";
    pub const ZCODE_FILE_LOCK_TIMEOUT: &str = "ZCODE_FILE_LOCK_TIMEOUT";
    pub const ZCODE_USER_DIALOG_TIMEOUT_MS: &str = "ZCODE_USER_DIALOG_TIMEOUT_MS";

    // 请求、工具与 ToolSearch 的覆盖（cli-args.md §9）。
    pub const ZCODE_MAX_OUTPUT_TOKENS: &str = "ZCODE_MAX_OUTPUT_TOKENS";
    pub const MAX_THINKING_TOKENS: &str = "MAX_THINKING_TOKENS";
    pub const ENABLE_TOOL_SEARCH: &str = "ENABLE_TOOL_SEARCH";
    pub const BASH_DEFAULT_TIMEOUT_MS: &str = "BASH_DEFAULT_TIMEOUT_MS";
    pub const BASH_MAX_TIMEOUT_MS: &str = "BASH_MAX_TIMEOUT_MS";
    pub const ZCODE_FORK_SUBAGENT: &str = "ZCODE_FORK_SUBAGENT";
    pub const IS_SANDBOX: &str = "IS_SANDBOX";
    pub const ZCODE_ENABLE_TASKS: &str = "ZCODE_ENABLE_TASKS";
    pub const ZCODE_ENABLE_TODO_TOOLS: &str = "ZCODE_ENABLE_TODO_TOOLS";

    // 压缩与记忆（compact.md §2、memory.md）。
    pub const DISABLE_COMPACT: &str = "DISABLE_COMPACT";
    pub const DISABLE_AUTO_COMPACT: &str = "DISABLE_AUTO_COMPACT";
    pub const ZCODE_AUTOCOMPACT_PCT_OVERRIDE: &str = "ZCODE_AUTOCOMPACT_PCT_OVERRIDE";
    pub const ZCODE_AUTO_COMPACT_WINDOW: &str = "ZCODE_AUTO_COMPACT_WINDOW";
    pub const ZCODE_DISABLE_AUTO_MEMORY: &str = "ZCODE_DISABLE_AUTO_MEMORY";

    // 流空闲看门狗（stall-visibility.md §2.2）。
    pub const ZCODE_STREAM_IDLE_TIMEOUT_MS: &str = "ZCODE_STREAM_IDLE_TIMEOUT_MS";
    pub const ZCODE_BYTE_STREAM_IDLE_TIMEOUT_MS: &str = "ZCODE_BYTE_STREAM_IDLE_TIMEOUT_MS";
    pub const ZCODE_ENABLE_STREAM_WATCHDOG: &str = "ZCODE_ENABLE_STREAM_WATCHDOG";
    pub const ZCODE_ENABLE_BYTE_WATCHDOG: &str = "ZCODE_ENABLE_BYTE_WATCHDOG";

    // MCP（ext.md）。
    pub const ZCODE_MCP_STARTUP_WAIT_MS: &str = "ZCODE_MCP_STARTUP_WAIT_MS";
    pub const MCP_CONNECTION_NONBLOCKING: &str = "MCP_CONNECTION_NONBLOCKING";
    pub const MCP_CONNECT_TIMEOUT_MS: &str = "MCP_CONNECT_TIMEOUT_MS";
    pub const MCP_TIMEOUT: &str = "MCP_TIMEOUT";
    pub const MCP_TOOL_TIMEOUT: &str = "MCP_TOOL_TIMEOUT";
    pub const MAX_MCP_OUTPUT_TOKENS: &str = "MAX_MCP_OUTPUT_TOKENS";

    // 账号服务的地址与产品环境（platform.md §5.4，cli `origins.rs`）。
    pub const ZCODE_ENV: &str = "ZCODE_ENV";
    pub const ZCODE_BASE_URL: &str = "ZCODE_BASE_URL";
    pub const ZCODE_ENDPOINT_ORIGIN: &str = "ZCODE_ENDPOINT_ORIGIN";
    pub const ZCODE_PRODUCTION_BASE_URL: &str = "ZCODE_PRODUCTION_BASE_URL";
    pub const ZCODE_TEST_BASE_URL: &str = "ZCODE_TEST_BASE_URL";
    pub const BIGMODEL_API_BASE_URL: &str = "BIGMODEL_API_BASE_URL";
    pub const BIGMODEL_PRODUCTION_API_BASE_URL: &str = "BIGMODEL_PRODUCTION_API_BASE_URL";
    pub const BIGMODEL_TEST_API_BASE_URL: &str = "BIGMODEL_TEST_API_BASE_URL";
    pub const ZAI_BUSINESS_BASE_URL: &str = "ZAI_BUSINESS_BASE_URL";
    pub const ZAI_PRODUCTION_BUSINESS_BASE_URL: &str = "ZAI_PRODUCTION_BUSINESS_BASE_URL";
    pub const ZAI_TEST_BUSINESS_BASE_URL: &str = "ZAI_TEST_BUSINESS_BASE_URL";

    // 自动更新与包管理器（update.md）。
    pub const ZCODE_MANAGED_BY_PNPM: &str = "ZCODE_MANAGED_BY_PNPM";
    pub const ZCODE_MANAGED_BY_NPM: &str = "ZCODE_MANAGED_BY_NPM";
    pub const ZCODE_MANAGED_BY_BUN: &str = "ZCODE_MANAGED_BY_BUN";
    pub const ZCODE_MANAGED_PACKAGE: &str = "ZCODE_MANAGED_PACKAGE";
    pub const ZCODE_MANAGED_PLATFORM_PACKAGE: &str = "ZCODE_MANAGED_PLATFORM_PACKAGE";
    pub const ZCODE_MANAGED_UPDATE_HANDOFF: &str = "ZCODE_MANAGED_UPDATE_HANDOFF";

    // TUI 的设置（tui.md）。
    pub const ZCODE_SCROLL_SPEED: &str = "ZCODE_SCROLL_SPEED";
    pub const ZCODE_AFK_TIMEOUT_MS: &str = "ZCODE_AFK_TIMEOUT_MS";
    pub const ZCODE_AFK_COUNTDOWN_MS: &str = "ZCODE_AFK_COUNTDOWN_MS";

    // Shell 与执行环境（tools-files.md、embedded-search.md）。
    pub const ZCODE_SHELL: &str = "ZCODE_SHELL";
    pub const ZCODE_GIT_BASH_PATH: &str = "ZCODE_GIT_BASH_PATH";
    pub const ZCODE_EXECPATH: &str = "ZCODE_EXECPATH";

    // 只给测试用的声明（只在 debug 构建读，model.md §6、remote-exec.md）。
    pub const _ZCODE_ASSUME_FIRST_PARTY_BASE_URL: &str = "_ZCODE_ASSUME_FIRST_PARTY_BASE_URL";
    pub const _ZCODE_ASSUME_TOOL_SEARCH: &str = "_ZCODE_ASSUME_TOOL_SEARCH";
    pub const _ZCODE_ASSUME_MONITOR_OFF: &str = "_ZCODE_ASSUME_MONITOR_OFF";
    pub const _ZCODE_ASSUME_LOOPBACK_EXECUTOR: &str = "_ZCODE_ASSUME_LOOPBACK_EXECUTOR";
    pub const _ZCODE_ASSUME_LOOPBACK_MACHINES: &str = "_ZCODE_ASSUME_LOOPBACK_MACHINES";
    pub const _ZCODE_ASSUME_SSH_MACHINES: &str = "_ZCODE_ASSUME_SSH_MACHINES";
    pub const _ZCODE_ASSUME_SSH_CONFIG: &str = "_ZCODE_ASSUME_SSH_CONFIG";
    pub const _ZCODE_ASSUME_SSH_ARTIFACT: &str = "_ZCODE_ASSUME_SSH_ARTIFACT";
    pub const _ZCODE_ASSUME_SSH_EXECUTOR: &str = "_ZCODE_ASSUME_SSH_EXECUTOR";
    pub const _ZCODE_ASSUME_BASE_URL: &str = "_ZCODE_ASSUME_BASE_URL";
    pub const _ZCODE_ASSUME_NPM_REGISTRY: &str = "_ZCODE_ASSUME_NPM_REGISTRY";
    pub const _ZCODE_ASSUME_RULE_MODEL: &str = "_ZCODE_ASSUME_RULE_MODEL";

    // 网络：代理与证书（net、cli `network.rs`；子进程照传）。
    pub const ZCODE_HTTP_PROXY: &str = "ZCODE_HTTP_PROXY";
    pub const ZCODE_NO_PROXY: &str = "ZCODE_NO_PROXY";
    pub const ZCODE_AGENT_CA_CERT: &str = "ZCODE_AGENT_CA_CERT";
    pub const HTTP_PROXY: &str = "HTTP_PROXY";
    pub const HTTPS_PROXY: &str = "HTTPS_PROXY";
    pub const NO_PROXY: &str = "NO_PROXY";
    pub const ALL_PROXY: &str = "ALL_PROXY";
    pub const NODE_EXTRA_CA_CERTS: &str = "NODE_EXTRA_CA_CERTS";
    pub const SSL_CERT_FILE: &str = "SSL_CERT_FILE";
    pub const SSL_CERT_DIR: &str = "SSL_CERT_DIR";

    // 终端与语言（TUI 的探测与 i18n、请求头的语言）。
    pub const TERM: &str = "TERM";
    pub const COLORTERM: &str = "COLORTERM";
    pub const TERM_PROGRAM: &str = "TERM_PROGRAM";
    pub const TERM_PROGRAM_VERSION: &str = "TERM_PROGRAM_VERSION";
    pub const TERMINAL_EMULATOR: &str = "TERMINAL_EMULATOR";
    pub const KITTY_WINDOW_ID: &str = "KITTY_WINDOW_ID";
    pub const WT_SESSION: &str = "WT_SESSION";
    pub const KONSOLE_VERSION: &str = "KONSOLE_VERSION";
    pub const GNOME_TERMINAL_SERVICE: &str = "GNOME_TERMINAL_SERVICE";
    pub const XTERM_VERSION: &str = "XTERM_VERSION";
    pub const VTE_VERSION: &str = "VTE_VERSION";
    pub const ALACRITTY_LOG: &str = "ALACRITTY_LOG";
    pub const LC_TERMINAL: &str = "LC_TERMINAL";
    pub const CURSOR_TRACE_ID: &str = "CURSOR_TRACE_ID";
    pub const VSCODE_GIT_ASKPASS_MAIN: &str = "VSCODE_GIT_ASKPASS_MAIN";
    pub const TMUX: &str = "TMUX";
    pub const STY: &str = "STY";
    pub const ZELLIJ: &str = "ZELLIJ";
    pub const SSH_CONNECTION: &str = "SSH_CONNECTION";
    pub const WSL_DISTRO_NAME: &str = "WSL_DISTRO_NAME";
    pub const DISPLAY: &str = "DISPLAY";
    pub const WAYLAND_DISPLAY: &str = "WAYLAND_DISPLAY";
    pub const LANG: &str = "LANG";
    pub const LANGUAGE: &str = "LANGUAGE";
    pub const LC_ALL: &str = "LC_ALL";
    pub const LC_MESSAGES: &str = "LC_MESSAGES";
    pub const TZ: &str = "TZ";

    // 用户、目录与系统（home、临时目录、Windows 的系统变量）。
    pub const HOME: &str = "HOME";
    pub const USERPROFILE: &str = "USERPROFILE";
    pub const USER: &str = "USER";
    pub const USERNAME: &str = "USERNAME";
    pub const LOGNAME: &str = "LOGNAME";
    pub const TMPDIR: &str = "TMPDIR";
    pub const TEMP: &str = "TEMP";
    pub const TMP: &str = "TMP";
    pub const SystemRoot: &str = "SystemRoot";
    pub const windir: &str = "windir";
    pub const ComSpec: &str = "ComSpec";
    pub const PATHEXT: &str = "PATHEXT";
    pub const APPDATA: &str = "APPDATA";
    pub const LOCALAPPDATA: &str = "LOCALAPPDATA";
    pub const ProgramFiles: &str = "ProgramFiles";
    pub const NUMBER_OF_PROCESSORS: &str = "NUMBER_OF_PROCESSORS";
    pub const PROCESSOR_ARCHITECTURE: &str = "PROCESSOR_ARCHITECTURE";

    // 子进程（shell、git、编辑器）的环境。
    pub const PATH: &str = "PATH";
    pub const SHELL: &str = "SHELL";
    pub const PWD: &str = "PWD";
    pub const OLDPWD: &str = "OLDPWD";
    pub const SHLVL: &str = "SHLVL";
    pub const EDITOR: &str = "EDITOR";
    pub const VISUAL: &str = "VISUAL";
    pub const PAGER: &str = "PAGER";
    pub const BASH_ENV: &str = "BASH_ENV";
    pub const GIT_EDITOR: &str = "GIT_EDITOR";
    pub const GIT_ASKPASS: &str = "GIT_ASKPASS";
    pub const GIT_TERMINAL_PROMPT: &str = "GIT_TERMINAL_PROMPT";
    pub const GIT_ALLOW_PROTOCOL: &str = "GIT_ALLOW_PROTOCOL";
    pub const GIT_CONFIG_PARAMETERS: &str = "GIT_CONFIG_PARAMETERS";
    pub const GIT_NO_LAZY_FETCH: &str = "GIT_NO_LAZY_FETCH";
    pub const GIT_NO_REPLACE_OBJECTS: &str = "GIT_NO_REPLACE_OBJECTS";
    pub const GIT_LFS_SKIP_SMUDGE: &str = "GIT_LFS_SKIP_SMUDGE";
    pub const GIT_SSH_COMMAND: &str = "GIT_SSH_COMMAND";
    pub const SSH_AUTH_SOCK: &str = "SSH_AUTH_SOCK";

    // 开发工具的目录（Bash 的环境照传）。
    pub const GOPATH: &str = "GOPATH";
    pub const GOROOT: &str = "GOROOT";
    pub const CARGO_HOME: &str = "CARGO_HOME";
    pub const RUSTUP_HOME: &str = "RUSTUP_HOME";
    pub const JAVA_HOME: &str = "JAVA_HOME";
    pub const NVM_DIR: &str = "NVM_DIR";
    pub const PNPM_HOME: &str = "PNPM_HOME";
    pub const PYENV_ROOT: &str = "PYENV_ROOT";
    pub const VIRTUAL_ENV: &str = "VIRTUAL_ENV";
    pub const CONDA_PREFIX: &str = "CONDA_PREFIX";
    pub const HOMEBREW_PREFIX: &str = "HOMEBREW_PREFIX";
}

/// managed 规则与 skill 所在的系统目录（按 Node 的平台名）：macOS `/Library/Application Support/ZCode`、
/// Windows `C:\ProgramData\ZCode`、其余 `/etc/zcode`（README §2；context 的规则文件与 ext 的 skill 共用）。
pub fn managed_dir(platform: &str) -> &'static str {
    match platform {
        "darwin" => "/Library/Application Support/ZCode",
        "win32" => r"C:\ProgramData\ZCode",
        _ => "/etc/zcode",
    }
}

#[cfg(test)]
#[path = "names_tests.rs"]
mod tests;
