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
