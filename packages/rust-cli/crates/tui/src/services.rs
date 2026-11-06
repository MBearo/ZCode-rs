//! 入口注入的服务（依赖注入，docs/specs/rust-cli/tui.md §7.1、§7.2）：TUI 不读写文件、不起进程，界面语言的
//! 持久化、输入历史的追加与账号的登录、退出由入口经 platform 实现；测试用替身。TUI 不依赖 platform 与 runtime（F133，
//! 审查 E7）。端口的 trait 与类型在 `rust-cli-tui-ports`（tui-kit.md §2.2），这里再导出；本界面自己的剪贴板、历史与
//! 自动压缩的端口与 `Services` 留在这里。
use std::sync::Arc;

use futures_util::future::BoxFuture;
use rust_cli_tui_kit::app::widgets::usage::{
    PlanDetail, PlanQuota, UsageModel, UsagePlan, UsageRange,
};
pub use rust_cli_tui_ports::services::{
    Account, Authorize, CancellationToken, CockpitNote, CockpitPort, Configured, CopyRoute, Editor,
    Family, FamilyModel, FamilyStatus, ImageBytes, LocaleStore, LoginFailure, MemoryFile,
    PlanConnection, Preferences, RedactedString, StoreFile, Stores, TeamStatus, UiLocale,
    WorkspaceTrust,
};

/// 输入历史的追加（tui.md「输入历史」）：发送时调用，不等待写完；入口唯一的写者按调用顺序追加到历史文件。`at_ms` 是
/// 发送时刻（毫秒）。
pub trait HistoryRecorder: Send + Sync {
    fn record(&self, text: &str, at_ms: u64);
}

/// 配置的 `compact.autoThresholdTokens`（compact.md §2.3）的那一份：进程里一份，内核每次判定现读，设置写回后换掉它。
/// 入口把内核的那一份包成这个端口（TUI 不依赖 runtime，F133）。
pub trait AutoCompactCell: Send + Sync {
    fn get(&self) -> Option<u64>;
    fn set(&self, at: Option<u64>);
}

/// 剪贴板与拖进来的图片文件（tui-input.md §3.1）：读写系统剪贴板要起进程（macOS `osascript` / `pbcopy`、Linux
/// `wl-paste` / `xclip` 等、Windows PowerShell），读本机文件要做 IO，都由入口实现；测试用替身。没有图片时是 `Ok(None)`。
pub trait Clipboard: Send + Sync {
    fn image(&self) -> BoxFuture<'static, Result<Option<ImageBytes>, String>>;

    /// 粘贴的文字是一个图片文件的路径（拖文件进终端）时读出它；不是文件或读不出时 `Ok(None)`。
    fn image_file(&self, path: String) -> BoxFuture<'static, Result<Option<ImageBytes>, String>>;

    /// 复制文字（F25，tui-select.md §3.2，起进程的部分）：不在 SSH 里时写系统剪贴板，在 tmux 里
    /// 另写 tmux 的缓冲区；OSC 52 由 TUI 自己写到终端。返回复制的途径。缺省（没有系统剪贴板）只有 OSC 52。
    fn copy_text(&self, _text: String) -> BoxFuture<'static, CopyRoute> {
        Box::pin(async { CopyRoute::Osc52 })
    }
}

/// `/usage` 的套餐用量（Puffin `SettingsHost::usage` 的三个问题，platform.md §11）：入口经 platform 回答（cli 的
/// `tui_usage.rs`：一分钟内的结果直接给出，同一个问题同时只有一个请求）。答的是 kit 的类型。
pub trait PlanUsage: Send + Sync {
    /// 当前模型的 provider 的计划，与当前模型。
    fn plans(
        &self,
        provider: String,
    ) -> BoxFuture<'static, Result<(UsageModel, Vec<UsagePlan>), String>>;
    fn quota(&self, plan: String) -> BoxFuture<'static, Result<PlanQuota, String>>;
    fn detail(
        &self,
        plan: String,
        range: UsageRange,
    ) -> BoxFuture<'static, Result<PlanDetail, String>>;
}

/// 入口给 TUI 的配置值：启动时读一次，之后不变（架构整理 U13：以前与服务混放在 `Services` 里）。
#[derive(Clone, Default)]
pub struct UiConfig {
    /// 配置文件的 `ui.locale`；没有时按系统语言。
    pub locale: Option<UiLocale>,
    /// `/memory` 的文件（入口按规则文件的位置给出）。
    pub memory_files: Vec<MemoryFile>,
    /// npm 上有更新的版本：欢迎块的版本行尾说 `X is out · zcode update`（docs/specs/rust-cli/update.md §7，F48）。
    pub update: Option<crate::update::UpdateInfo>,
    /// 提问框的空闲超时（配置的 `askUserQuestionTimeout` 与 `ZCODE_AFK_*`，tools-more.md §1.1，工作线 F72）；
    /// 没有时不计时。
    pub question_timeout: Option<crate::ui::afk::QuestionTimeout>,
    /// 绑定到远端机器的 MCP 服务器（服务器名，机器名；R5，remote-exec.md §22.8）：MCP 工具行标出机器。入口从同一份
    /// MCP 设置给出。
    pub mcp_machines: Vec<(String, String)>,
    /// 配置文件来源的权限规则来自的文件（来源、种类、绝对路径）：设置的 Permissions 段在编辑器里打开它们。
    pub rule_files: Vec<(
        rust_cli_kernel_api::permission::RuleSource,
        rust_cli_kernel_api::permission::RuleBehavior,
        String,
    )>,
    /// `--add-dir` 加的文件夹（设置的 Permissions 段列出）。
    pub add_dirs: Vec<String>,
    /// `!` 命令跑在哪个 shell 里（`$SHELL` 的文件名，Puffin `shell_name`）；没有时 `sh`。第一次进 shell 模式时输入框上方说它。
    pub shell_name: Option<String>,
    /// 在远程 shell 里（入口按 Puffin 的 `place`：有 SSH，或 Linux 上没有图形界面）：门道的登录把网址整段折出，不等浏览器
    /// （Puffin `place.remote`，tui-kit.md §4.2「登录」）。
    pub remote_shell: bool,
}

/// [`Services::file_len`]。
pub type FileLen = Arc<dyn Fn(String) -> BoxFuture<'static, Option<u64>> + Send + Sync>;

/// [`Services::peek_file`]。
pub type PeekFile = Arc<
    dyn Fn(
            rust_cli_kernel_api::env::WorkspaceRef,
            String,
        ) -> BoxFuture<'static, rust_cli_tui_kit::app::widgets::at_panel::data::PathPeek>
        + Send
        + Sync,
>;

/// 入口注入的服务。
///
/// 架构整理 U13：以前 7 个服务都是 `Option`，入口除了远端机器都给了，`None` 的分支（“不可用”的提示、不写配置、不做
/// 信任检查）只有测试走到；现在这 7 个里只有远端机器可以没有。本界面另有的端口（小文件、驾驶舱、套餐用量、文件的
/// 大小与预览、自动压缩）没有时各自退回本次运行有效或不显示，照旧是 `Option`。
#[derive(Clone)]
pub struct Services {
    /// `/locale <值>` 的写回。
    pub locale_store: Arc<dyn LocaleStore>,
    /// `/login`、`/logout`。
    pub account: Arc<dyn Account>,
    /// 粘贴图片与复制（tui-input.md §3.1、tui-select.md §3.2）。
    pub clipboard: Arc<dyn Clipboard>,
    /// ctrl+g 与 `/memory`（tui-input.md §4）。
    pub editor: Arc<dyn Editor>,
    /// `/effort`、`/model` 的缺省、`/mcp enable/disable` 与自动压缩阈值的写回（主题不经它：`/theme` 写 `theme.json`，
    /// tui-kit.md §3.4）。
    pub preferences: Arc<dyn Preferences>,
    /// 切到别的项目之前的信任检查（workspace-trust.md §2.8）。
    pub trust: Arc<dyn WorkspaceTrust>,
    /// 远端机器（remote-exec.md §21.2）：`/machines` 与机器状态行；没有时 `/machines` 提示不可用、状态行不出现。
    pub machines: Option<Arc<dyn crate::machines::Machines>>,
    /// Puffin 的小文件（`theme.json`，与 Puffin 同一个文件，tui-kit.md §2.3、§3.4）：启动时读主题的选择，`/theme` 选定时
    /// 写回；没有时用缺省（Terminal），`/theme` 只对本次运行有效。
    pub stores: Option<Arc<dyn Stores>>,
    /// 驾驶舱的笔记、监听与 tmux（tui-cockpit.md §1）；没有时页上没有“新的”，也不能去别的窗格。
    pub cockpit: Option<Arc<dyn CockpitPort>>,
    /// 配置的 `compact.autoThresholdTokens`（compact.md §2.3）与把它钉住的文件（项目或环境写了它时，设置页写了也不生效）；
    /// 设置的 General 段读写它。没有时那一行不出现。
    pub compact_at: Option<(Arc<dyn AutoCompactCell>, Option<String>)>,
    /// 一个文件的字节数，不存在时 `None`（设置的 Memory 段写规则文件的大小；界面不做文件 IO）。没有时都当作不存在。
    pub file_len: Option<FileLen>,
    /// `/usage` 的套餐用量；没有时卡说读不到（同 cli host 的「本版不可用」）。
    pub plan_usage: Option<Arc<dyn PlanUsage>>,
    /// `@` 面板的预览读一个文件（工作区，相对它的路径；Puffin host 的 `peek`）：行数、大小与前 4 行；入口只读本机工作区里的，
    /// 远端的回答 `Missing`。没有时预览只有名字与路径。
    pub peek_file: Option<PeekFile>,
}
