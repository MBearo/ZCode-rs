//! 入口注入、终端界面用的服务（依赖注入，docs/specs/rust-cli/tui.md §7.1、§7.2；tui-kit.md §2.2）：界面不读写
//! 文件、不起进程，界面语言的持久化、账号的登录与退出、外部编辑器、工作区信任的判定由入口经 platform / fs / proc 实现；测试用
//! 替身。剪贴板的端口在界面的 crate。
use std::collections::HashMap;

use futures_util::future::BoxFuture;
use futures_util::stream::BoxStream;
pub use rust_cli_base::account::Family;
pub use rust_cli_base::secret::RedactedString;
pub use rust_cli_base::ui_prefs::UiLocale;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::peers::TmuxPlace;
use tokio::sync::oneshot;
pub use tokio_util::sync::CancellationToken;

/// 界面语言的持久化：写回 Node 共用的用户级配置文件 `ui.locale`。成功时给出文件路径，失败时给出原因。
pub trait LocaleStore: Send + Sync {
    fn save(&self, locale: UiLocale) -> BoxFuture<'static, Result<String, String>>;
}

/// 界面偏好的持久化（docs/specs/rust-cli/tui-manage.md §2）：`/effort` 把当前模型与档位写成新会话的缺省（个人配置的
/// `defaultModelSelection`）。成功时给出文件路径，失败时给出原因。主题不再经这里：两套界面都写 `theme.json`
/// （[`Stores`]，tui-kit.md §3.4），`ui.theme` 的写回端口已删。
pub trait Preferences: Send + Sync {
    fn save_default_model(
        &self,
        selection: rust_cli_kernel_api::model::ModelSelection,
    ) -> BoxFuture<'static, Result<String, String>>;

    /// `/mcp enable/disable`（tui-manage.md §6.1）：把 `enabled` 写进定义这个服务器的配置文件。`Ok(None)`：它不在
    /// 配置文件里（`--mcp-config`、插件），只改本次运行。
    fn save_mcp_enabled(
        &self,
        _server: String,
        _enabled: bool,
    ) -> BoxFuture<'static, Result<Option<String>, String>> {
        Box::pin(async { Ok(None) })
    }

    /// Puffin 的 `/autocompact` 与设置页（compact.md §2.3）：把 `compact.autoThresholdTokens` 写进用户级配置，`None`
    /// 删掉它。返回写入的文件。缺省不能写。
    fn save_compact_at(&self, _at: Option<u64>) -> BoxFuture<'static, Result<String, String>> {
        Box::pin(async { Err("This isn't available in this version yet".to_owned()) })
    }
}

/// 浏览器授权的地址（`begin_login` 之后、轮询之前交给界面）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Authorize {
    pub url: String,
    /// 打开浏览器失败的原因（Node 在结果末尾提示 `Browser open failed`）。
    pub browser_failed: Option<String>,
}

/// 登录或手工 key 配置成功。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Configured {
    pub family: Family,
    /// 浏览器登录的用户名（`name`、`email`、`user_id` 中第一个非空的）；手工 key 没有。
    pub user: Option<String>,
    /// `<provider>/<model>`：登录写进个人配置的默认模型。
    pub model: String,
    /// 凭据文件（浏览器登录时显示）。
    pub credentials: Option<String>,
    /// 个人配置文件。
    pub config: String,
    pub browser_failed: Option<String>,
    /// 经浏览器授权（否则是手工 key）。
    pub oauth: bool,
}

/// 登录失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginFailure {
    /// 用户取消（esc），没有写任何文件。
    Cancelled,
    /// 其他失败：原因原样显示。
    Failed(String),
}

/// 账号的登录与退出（platform.md §5.6）。取消是一条消息：`cancel` 被取消后，实现在下一次检查时停下并返回
/// [`LoginFailure::Cancelled`]，调用方不丢弃 future。
pub trait Account: Send + Sync {
    /// 浏览器授权：拿到地址后经 `authorize` 交给界面（打开浏览器由实现负责），再轮询到结果。
    fn login(
        &self,
        family: Family,
        authorize: oneshot::Sender<Authorize>,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<Configured, LoginFailure>>;

    /// 手工 key（platform 不验证 key）。
    fn configure_api_key(
        &self,
        family: Family,
        key: RedactedString,
    ) -> BoxFuture<'static, Result<Configured, String>>;

    /// 退出 Coding Plan 账号，返回凭据文件的路径。
    fn logout(&self) -> BoxFuture<'static, Result<String, String>>;

    /// 内置家族的登录状态（Settings › Models 的家族行与家族页，tui-kit.md §4.5）。缺省什么也不知道：Models 段
    /// 不列家族。
    fn families(&self) -> BoxFuture<'static, Vec<FamilyStatus>> {
        Box::pin(async { Vec::new() })
    }
}

/// 一个内置家族的登录状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FamilyStatus {
    pub family: Family,
    /// 家族的名字（`Z.ai`）。
    pub name: String,
    pub signed_in: bool,
    /// 登录的账号（邮箱或名字）；不知道时为 ""。
    pub account: String,
    pub connection: PlanConnection,
    /// 账号的团队计划；选中的那个在 `connection` 里按位置。
    pub teams: Vec<TeamStatus>,
    /// 读不出团队时的原因；没有时为 ""。
    pub teams_error: String,
    /// 模型目录里这个家族的 provider：家族页列它的模型。
    pub provider: String,
    /// 这个 provider 列出的全部内置模型，关着的也在（platform `family_accounts`，与 Puffin 的 host 同一份）。
    pub models: Vec<FamilyModel>,
}

/// 家族页上的一个模型（Puffin `ModelSettings` 的这几项）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FamilyModel {
    pub id: String,
    pub enabled: bool,
    pub context_window: Option<u64>,
}

/// 家族怎么为请求付费（Puffin `FamilyConnection`）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PlanConnection {
    Start,
    #[default]
    Individual,
    /// `teams` 里第几个团队。
    Team(usize),
}

/// 账号的一个团队计划（Puffin `TeamPlan`）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TeamStatus {
    /// 组织名；不知道时为 ""。
    pub name: String,
    /// 项目的密钥拿得到。
    pub usable: bool,
    /// 不可用时的说明；没有时为 ""。
    pub why: String,
}

/// 读到的图片：原始字节（界面判断格式并编码，tui-input.md §3.1）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageBytes {
    pub bytes: Vec<u8>,
}

/// 复制到了哪里（docs/specs/rust-cli/tui-select.md §3.2）：决定复制后的提示文字。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyRoute {
    /// 系统剪贴板命令（pbcopy、wl-copy / xclip / xsel、PowerShell）。
    Native,
    /// tmux 的缓冲区（SSH 里或本机没有剪贴板命令时）。
    TmuxBuffer,
    /// 只有终端的 OSC 52（无法确认）。
    Osc52,
}

/// 外部编辑器（tui-input.md §4）：写读临时文件、起编辑器进程都由入口做。调用时 TUI 已交出终端，编辑器继承
/// 终端；返回后 TUI 收回终端。失败的原因原样显示（`<名字> exited with code N`）。
pub trait Editor: Send + Sync {
    /// 编辑一段文字，返回编辑后的全文。
    fn edit(&self, text: String) -> BoxFuture<'static, Result<String, String>>;

    /// 打开一个文件（不存在时先建空文件，`/memory`），等编辑器结束。
    fn open(&self, path: String) -> BoxFuture<'static, Result<(), String>>;

    /// 编辑器来自哪个环境变量与它的值（`/memory` 的提示）：`("$VISUAL", "code")`；用缺省编辑器时 None。
    fn source(&self) -> Option<(String, String)>;
}

/// `/memory` 列出的一个规则或记忆文件（tui-input.md §4.2）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryFile {
    /// 列表里的名字（原文，例如 `User memory`）。
    pub label: String,
    /// 显示用的路径（家目录写成 `~`）。
    pub display: String,
    /// 交给编辑器的路径。
    pub path: String,
}

/// 工作区信任（修复线 F29，docs/specs/rust-cli/workspace-trust.md §2.8）：切到别的项目之前判定那个目录。判定与写记录在
/// 入口（与启动时同一段代码），界面只画对话框。
pub trait WorkspaceTrust: Send + Sync {
    /// 已信任（直接或经父目录）时为 None；否则是要问的内容（不带项目授权的后备询问）。
    fn check(
        &self,
        workspace: rust_cli_kernel_api::env::WorkspaceRef,
    ) -> BoxFuture<'static, Option<crate::trust::TrustPrompt>>;
    /// 用户同意：写项目键的记录（home 只本进程有效）；写失败只记日志。
    fn accept(&self, workspace: rust_cli_kernel_api::env::WorkspaceRef) -> BoxFuture<'static, ()>;
}

/// Puffin 的三个小文件（docs/puffin-tui.md §4.2「主题」、「新会话的模型」、权限模式的记忆）：`theme.json`、`models.json`、
/// `modes.json`，读与原子写都在入口。`read` 文件不存在时 `Ok(None)`。从 Puffin 界面搬来（docs/specs/rust-cli/tui-kit.md
/// §2.1）：旧界面也读写 `theme.json`（§3.4），实现在 cli。
pub trait Stores: Send + Sync {
    fn read(&self, file: StoreFile) -> BoxFuture<'static, Result<Option<String>, String>>;
    fn write(&self, file: StoreFile, text: String) -> BoxFuture<'static, Result<(), String>>;
}

/// [`Stores`] 的一个文件。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StoreFile {
    Theme,
    Models,
    Modes,
    /// 驾驶舱记着的分组（`puffin-tui/cockpit.json`，tui-cockpit.md §2.1）。
    Cockpit,
}

/// 驾驶舱在本机的部分（tui-cockpit.md §1）：笔记、监听与 tmux。
pub trait CockpitPort: Send + Sync {
    /// 登记处或笔记变了；建好监听时先出一项。丢掉流就注销。
    fn changes(&self) -> BoxStream<'static, ()>;
    /// 这些会话的笔记。
    fn notes(&self, ids: Vec<SessionId>) -> BoxFuture<'static, HashMap<SessionId, CockpitNote>>;
    fn mark_seen(&self, id: SessionId, at_ms: u64) -> BoxFuture<'static, ()>;
    fn write_turn(
        &self,
        id: SessionId,
        ended_at_ms: u64,
        failed: Option<String>,
    ) -> BoxFuture<'static, ()>;
    /// 删除会话时两个笔记一起删。
    fn forget(&self, id: SessionId) -> BoxFuture<'static, ()>;
    /// 切到那个窗格（`tmux switch-client -t`）。
    fn go_to(&self, place: TmuxPlace) -> BoxFuture<'static, Result<(), String>>;
    /// 这个进程所在的 tmux 的 socket；不在 tmux 里时为 None。
    fn tmux_socket(&self) -> Option<String>;
}

/// 一个会话的笔记。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CockpitNote {
    pub ended_at_ms: Option<u64>,
    /// 回合失败时的原因（`Failed` 的文字）。
    pub failed: Option<String>,
    pub seen_at_ms: Option<u64>,
}
