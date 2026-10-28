// 改写自 apps/zcode-host/crates/tui/src/ui/app.rs（feat/rs 79b6691c55）：会话内容换成 `view::Transcript`，
// Action 换成内核会话 API 的命令（docs/specs/rust-cli/tui.md §2、§4）。
//! 界面状态：会话内容只来自视图（`view::Transcript`）；这里只有草稿、视图、滚动、展开等界面状态，
//! 以及插在 transcript 里的本地 cell（提示、错误）。不做 IO：按键产生 [`Action`]，由 runner 执行。
use std::collections::HashSet;
use std::time::Instant;

use rust_cli_kernel_api::ids::{InteractionId, MessageId};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{Delivery, RewindScope};
use serde_json::Value;

use super::composer::Composer;
use super::i18n::{Lang, Strings};
use super::interaction::Interaction;
use super::list::ListView;
use super::mouse::{Click, ComposerDrag, Hits, ScreenSelection, Selection, Target};
use super::palette::Palette;
use crate::view::Transcript;

/// runner 要执行的动作（只经内核会话 API）。
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// 发送一条输入；没有会话时先建会话（tui.md §4.1）。
    Submit {
        text: String,
        delivery: Delivery,
    },
    /// 带图片的输入（tui-input.md §3）：文字块在前，图片在后。
    SubmitContent {
        content: Vec<rust_cli_kernel_api::message::ContentBlock>,
        delivery: Delivery,
    },
    /// ctrl+g、ctrl+x ctrl+e：用外部编辑器编辑输入（tui-input.md §4.1）。
    ExternalEditor,
    /// `/memory` 选中的文件：用外部编辑器打开（tui-input.md §4.2）；`display` 是提示里的路径。
    OpenMemory {
        path: String,
        display: String,
    },
    /// ctrl+v / alt+v：读剪贴板里的图片（tui-input.md §3.1）。
    PasteImage,
    /// 粘贴的文字是图片文件的路径：读出文件，不是图片时把 `text` 当文字粘贴。
    PasteImagePath {
        path: String,
        text: String,
    },
    /// bash 模式的输入（tui-input.md §2）。
    Bash {
        command: String,
        delivery: Delivery,
    },
    /// ctrl+enter（运行中）：低延迟投递，入队后把前台 Bash 转到后台、不中断（修复线 L-A，tui-input.md §11.1）。
    SendNow {
        text: String,
        delivery: Delivery,
    },
    /// ctrl+x ctrl+b（运行中）：前台 Bash 转到后台（修复线 L-A，tui-input.md §11.2）。
    BackgroundForeground,
    /// 输入框为空时的立即发送（修复线 F105，tui.md §5.1.1）：只把前台 Bash 转到后台，排着的插话在工具边界并入。
    DeliverQueued,
    Interrupt,
    Resolve {
        interaction: InteractionId,
        answer: Value,
    },
    /// 批准或拒绝扣住的同伴消息（cross-session.md §2.6）。
    ResolvePeer {
        message: String,
        approve: bool,
    },
    SetModel(ModelSelection),
    /// `/model`、meta+p 的选择：先换模型，被接受后再写成新工作区的默认模型（workspace-trust.md §3.2）。
    ChooseModel(ModelSelection),
    SetMode(PermissionMode),
    /// `/plan`：打开或关掉 plan 开关（修复线 F20）。
    SetPlan(bool),
    Compact(Option<String>),
    /// `/model`：读模型目录后打开列表。
    OpenModels,
    /// `/model 名称`：按模型目录解析后切换。
    PickModel(String),
    /// `/clear`：离开当前会话，下一次发送建新会话。
    Clear,
    /// 复制这段文字（系统剪贴板与 OSC 52，tui-select.md §3.2）。
    Copy(String),
    /// 复制鼠标选区：runner 先重画一次，再取选区文字（`clear` 为真时复制后清掉选区）。
    CopySelection {
        clear: bool,
    },
    /// ctrl+l：清屏并重画。
    Redraw,
    /// `/rewind` 或 esc 连按两次：取可选消息后打开选择器（session-lifecycle.md §5.4）。
    OpenRewind,
    /// 选择器确认：回退到 `target` 之前；对话被回退时把 `text` 放回输入框。
    Rewind {
        target: MessageId,
        scope: RewindScope,
        text: String,
    },
    /// `/resume`：列出会话后打开选择器。
    OpenResume,
    /// 切到这个会话。
    Resume(String),
    /// `/rename 名字`：改会话标题（来源记为 custom，docs/specs/rust-cli/titles.md §4）。
    Rename(String),
    /// 不带名字的 `/rename`：会话 actor 用模型起名（titles.md §8）。
    GenerateName,
    /// `/context [all]`：内核算出上下文用量后打开独立页面（context-usage.md §5.2、tui-panels.md §4）。
    Context {
        all: bool,
    },
    /// `/locale <值>`：写回配置文件，成功后切换界面语言（tui.md §7.1）。
    SetLocale(crate::services::UiLocale),
    /// 浏览器授权登录（tui.md §7.2）。
    Login(crate::services::Family),
    /// 取消在途的登录（一条消息）。
    CancelLogin,
    /// 保存手工 key（key 不经输入框，不进历史）。
    ConfigureKey {
        family: crate::services::Family,
        key: crate::services::RedactedString,
    },
    Logout,
    /// `/goal` 不带参数：取会话的目标后打开面板（goal.md §6.2）。
    ShowGoal,
    /// `/fork`：取可选的用户输入后打开选择器（session-lifecycle.md §6.5.3）。
    OpenFork,
    /// 选择器选中一条输入：新会话带到这一轮为止，成功后切过去。
    Fork {
        input: MessageId,
    },
    Quit,
    /// L28 的管理命令（docs/specs/rust-cli/tui-display.md）。
    Manage(super::manage::Manage),
    /// L30 的面板（docs/specs/rust-cli/tui-panels.md）。
    Panel(super::panels::Panel),
    /// ← 项目页与 `/resume` 页（修复线 F29，tui-panels.md §2、tui-manage.md §10）。
    Projects(super::projects::Request),
    /// `/machines` 的子命令与列表里的动作（远端执行 R4，remote-exec.md §21.3）：经入口的机器服务。
    Machines(super::machines::MachineRequest),
}

/// 插在 transcript 里的本地 cell（不属于会话事实）。
#[derive(Debug, Clone, PartialEq)]
pub enum Local {
    Info(String),
    Error(String),
    Status(Vec<(String, String)>),
    /// `/context [all]` 的独立页面（context-usage.md §5.2、tui-panels.md §4）；`all` 时展开明细。装箱：比其他
    /// 变体大得多。
    Context {
        usage: Box<rust_cli_kernel_api::context_usage::ContextUsage>,
        all: bool,
    },
    /// `/goal` 面板（goal.md §6.2）。
    Goal(Box<super::goal::Panel>),
    /// 标题加多行正文（`/usage`、后台任务的详情，tui-display.md）。
    Text {
        title: String,
        body: String,
    },
    /// （名称，说明，补充）。
    Table {
        title: String,
        rows: Vec<(String, String, String)>,
        empty: String,
    },
    /// npm 上有更新的版本（docs/specs/rust-cli/update.md §7，F48）：圆角框的提示。
    Update(Box<crate::update::UpdateInfo>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct LocalCell {
    /// 插入时 transcript 的最后一行；cell 显示在它之后。
    pub anchor: Option<u64>,
    pub local: Local,
}

/// 替换输入框的视图。
#[derive(Debug, Clone, PartialEq)]
pub enum Overlay {
    List(ListView),
    /// 独立页面（`/status`、`/help`）：内容不插进 transcript，esc 关闭。
    Page {
        local: Local,
        scroll: usize,
    },
    Shortcuts,
    /// ctrl+o：详细视图。
    Detail(super::detail::Detail),
    /// `/login`：登录视图（tui.md §7.2）。
    Login(super::login::LoginView),
    /// `/permissions`：规则页面（tui-manage.md §1.2）。装箱：比其他变体大得多。
    Permissions(Box<super::permissions::PermissionsView>),
    /// `/export`：导出方式与文件名（tui-manage.md §3）。
    Export(super::export::ExportView),
    /// `/diff`：当前改动与每回合的改动（tui-manage.md §4）。
    Diff(super::diff_view::DiffView),
    /// ← 项目页（修复线 F29，tui-panels.md §2）。装箱：比其他变体大得多。
    Projects(Box<super::projects::ProjectsPage>),
    /// `/resume` 页（修复线 F29，tui-manage.md §10）。
    Resume(Box<super::resume::ResumePage>),
    /// 切到别的项目之前的信任对话框（workspace-trust.md §2.8）。
    Trust(Box<super::trust_ask::TrustAsk>),
    /// Background 面板与 Shell 详情（tui-panels.md §3）：替换底部区域。
    Background(super::background::BackgroundView),
    /// `/usage` 的 Usage / Stats 页面（usage.md §4、§5）。装箱：统计数据比其他变体大得多。
    Usage(Box<super::usage::UsageView>),
    /// 切到完全访问的确认框（tui.md §7.8，工作线 F87）：替换底部区域，不是全屏视图。
    YoloConfirm(super::yolo_confirm::YoloConfirm),
    /// `/machines` 的列表（远端执行 R4，remote-exec.md §21.3）：替换底部区域。
    Machines(super::machines::MachinesView),
}

pub struct App {
    pub palette: Palette,
    pub lang: Lang,
    pub strings: &'static Strings,
    /// 会话的视图；没有会话时是新会话的草稿（模型与模式）。
    pub view: Transcript,
    /// 当前会话的 ID（`/status` 与退出提示用）；runner 打开会话后写入。
    pub session: Option<String>,
    /// 正在创建会话（第一次发送）。
    pub creating: bool,
    /// 创建期间的发送（tui.md §4.1）：会话打开、第一条发出后按顺序发给内核；显示在排队的输入后面。
    pub held: Vec<rust_cli_kernel_api::session_api::Command>,
    /// 界面层完成的本地命令的记录（`/effort`、`/plan` 等，修复线 L-A，tui-manage.md §11）：runner 每轮取走。
    pub records: Vec<rust_cli_kernel_api::user_command::LocalCommandRecord>,
    /// 运行中的 `/clear`、`/resume`（修复线 F38，tui.md §6；排进队列、不中断）：（显示的文字，动作），回合结束、
    /// 内核队列空了之后按顺序执行；显示在排队的输入后面。
    pub after_turn: Vec<(String, Action)>,
    pub composer: Composer,
    /// skill 清单第一次到之前提交、当时判定不了的命令（tui.md §6）：清单到了（`commands_arrived`）再提交一次。
    pub held_command: bool,
    pub overlay: Option<Overlay>,
    /// 当前待处理交互的选择状态；交互本身在视图里（旧会话的询问在 `detached` 里）。
    pub interaction: Option<Interaction>,
    /// 父会话关闭之后旧会话的询问（按到达顺序，agents.md §4.1.2，工作线 F72）：内核的 `detached_asks` 的投影。
    pub detached: Vec<rust_cli_kernel_api::orchestration::DetachedAsk>,
    /// 同伴消息批准框的焦点（0 拒绝，1 投递）；扣住的消息本身在视图里（cross-session.md §2.6）。
    pub peer_choice: usize,
    pub local: Vec<LocalCell>,
    /// 从底部向上滚过的行数；0 表示跟随最新内容。
    pub scroll: usize,
    /// 上滚期间又来了新内容。
    pub new_activity: bool,
    /// 上次排版的 transcript 总行数与可视高度。
    pub transcript_lines: usize,
    pub viewport: usize,
    /// 点击展开行单独展开的 cell（第一个行 id）。
    pub expanded_cells: HashSet<u64>,
    /// footer 的临时提示与等待第二次按下的按键。
    pub notice: Option<super::notice::Notice>,
    /// 最后一个会话事件到达的时间；状态行据此判断卡住。
    pub last_frame: Instant,
    /// 鼠标最后的位置（悬停的反馈）。
    pub hover: Option<(u16, u16)>,
    /// 家目录（显示路径时写成 `~`）；工作区本身只在 `view` 里（R1-3）。
    pub home: Option<String>,
    pub version: String,
    pub started: Instant,
    /// 输入框文字区的宽度（上下移动与折行用）；每帧由绘制更新。
    pub composer_width: usize,
    /// 上一帧的可点击区域与可见行。
    pub hits: Hits,
    pub selection: Option<Selection>,
    pub(crate) composer_drag: Option<ComposerDrag>,
    pub(crate) click: Option<Click>,
    pub(crate) press: Option<Target>,
    /// 全屏视图里的选区（屏幕坐标，tui-select.md §2.1）；与 `selection` 互斥。
    pub screen_selection: Option<ScreenSelection>,
    /// 最近一次复制的提示与时间（gap 行右侧显示，时长按途径）。
    pub copied: Option<(super::clipboard::Notice, Instant)>,
    /// 滚轮的加速状态（tui-select.md §4.2）；runner 按终端变量换成对应的参数。
    pub wheel: super::wheel::Accel,
    pub history_presses: usize,
    pub history_hinted: bool,
    /// rewind 选择器的条目（打开时由 runner 填入）。
    pub rewind: Vec<super::rewind::Entry>,
    /// ctrl+x 和弦的第一个键按下的时间（tui-input.md §5）。
    pub chord: Option<Instant>,
    /// `/memory` 列出的文件（入口给出，tui-input.md §4.2）。
    pub memory_files: Vec<crate::services::MemoryFile>,
    /// 任务清单的界面状态（ctrl+t，tui-display.md §2.2）。
    pub tasks: super::tasks::TaskPanel,
    /// 后台任务的最新快照（与 `agents` 同一份快照、同一个回执，runner 按请求序号写入，tui-display.md §2.3，修复线 F34）。
    pub background: Vec<rust_cli_kernel_api::manage::BackgroundEntry>,
    /// footer 的请求统计（TPS 与缓存命中率）的最后一份回执（runner 按请求序号写入，tui-display.md §2.6，F27）。
    pub request_stats: super::footer_stats::FooterStats,
    /// `/tasks` 列表的条目（打开时的快照；停止后按新快照刷新）。
    pub task_list: Vec<rust_cli_kernel_api::manage::BackgroundEntry>,
    /// 主题的当前值（配置的 `ui.theme` 或 `/theme` 选的，tui-manage.md §2.1）与启动时探测到的终端背景
    /// （`auto` 用它）。
    pub theme: crate::services::UiTheme,
    pub terminal_background: Option<super::palette::Rgb>,
    /// 终端通知的计时（tui-manage.md §5）。
    pub notify: super::notify::Notify,
    /// 提问框的空闲计时（tools-more.md §1.1，工作线 F72）。
    pub afk: super::afk::Afk,
    /// 子代理的最后一份快照（与 `background` 同一份快照，runner 按请求序号写入，tui-panels.md §0、§1）。
    pub agents: super::agents::AgentList,
    /// 正在查看的子代理（只读）。
    pub viewing: Option<super::agents::Viewing>,
    /// footer 下面获得焦点的东西（pill 或子代理列表）。
    pub focus: Option<super::agents::Focus>,
    /// 新会话的背景标志（tui.md §7.3，修复线 F26）：这一帧画在哪里与最近一次点击。
    pub logo: super::logo::Logo,
    /// 启动时要打开已有会话：“有新版本”的提示在第一次切换后移到新视图的顶上（docs/specs/rust-cli/update.md §7）。
    pub update_carry: Option<crate::update::UpdateInfo>,
    /// 按平台判断文字输入、松开事件与 alt 码、没有括号粘贴时的 paste burst（只在 Windows 上有，tui-input.md §9，
    /// 修复线 F65）；runner 按入口注入的平台设置（[`App::set_os`]）。
    pub typing: super::keyboard::Typing,
    pub releases: super::keyboard::Releases,
    pub burst: Option<super::paste_burst::PasteBurst>,
    /// 机器服务的最新快照（远端执行 R4，remote-exec.md §21.2；runner 在服务的变化通知之后重取）：状态行与 `/machines`。
    pub machines: Vec<crate::machines::MachineRow>,
    /// 入口注入了机器服务（没有时 `/machines` 提示不可用）。
    pub machines_enabled: bool,
    /// 绑定到远端机器的 MCP 服务器（规范化的服务器名，机器名；R5，remote-exec.md §22.8）：runner 启动时从服务取。
    pub mcp_machines: Vec<(String, String)>,
    /// 终端分辨得出增强按键（修复线 F104，tui-input.md §11.1.1）：进界面时键盘增强的查询结论，由 `Screen` 带进来；
    /// 决定立即发送的提示写 ctrl+enter 还是 ctrl+x ctrl+s。
    pub keyboard_enhanced: bool,
    pub quit: bool,
}

/// 待处理交互：（ID，请求）。
pub type Pending<'a> = (
    &'a rust_cli_kernel_api::ids::InteractionId,
    &'a rust_cli_kernel_api::orchestration::InteractionRequest,
);

/// [`App::pending`] 的实现：只借视图与旧会话的询问两个字段（调用方同时要改 `App` 的别的字段）。
pub fn pending_of<'a>(
    view: &'a Transcript,
    detached: &'a [rust_cli_kernel_api::orchestration::DetachedAsk],
) -> Option<Pending<'a>> {
    view.pending()
        .map(|(id, request)| (id, request))
        .or_else(|| detached.first().map(|ask| (&ask.interaction, &ask.request)))
}

impl App {
    pub fn new(
        palette: Palette,
        lang: Lang,
        view: Transcript,
        home: Option<String>,
        version: String,
    ) -> Self {
        Self {
            palette,
            lang,
            strings: super::i18n::strings(lang),
            view,
            session: None,
            creating: false,
            held: Vec::new(),
            records: Vec::new(),
            after_turn: Vec::new(),
            composer: Composer::default(),
            overlay: None,
            interaction: None,
            detached: Vec::new(),
            peer_choice: 0,
            local: Vec::new(),
            scroll: 0,
            new_activity: false,
            transcript_lines: 0,
            viewport: 0,
            expanded_cells: HashSet::new(),
            notice: None,
            last_frame: Instant::now(),
            hover: None,
            home,
            version,
            started: Instant::now(),
            composer_width: 80,
            hits: Hits::default(),
            selection: None,
            composer_drag: None,
            click: None,
            press: None,
            screen_selection: None,
            copied: None,
            wheel: super::wheel::Accel::default(),
            history_presses: 0,
            history_hinted: false,
            rewind: Vec::new(),
            chord: None,
            held_command: false,
            memory_files: Vec::new(),
            tasks: super::tasks::TaskPanel::default(),
            background: Vec::new(),
            request_stats: Default::default(),
            task_list: Vec::new(),
            theme: crate::services::UiTheme::Auto,
            terminal_background: palette.background,
            notify: super::notify::Notify::default(),
            afk: super::afk::Afk::default(),
            agents: super::agents::AgentList::default(),
            viewing: None,
            focus: None,
            logo: super::logo::Logo::default(),
            update_carry: None,
            typing: super::keyboard::Typing::default(),
            releases: super::keyboard::Releases::default(),
            burst: None,
            machines: Vec::new(),
            machines_enabled: false,
            mcp_machines: Vec::new(),
            keyboard_enhanced: false,
            quit: false,
        }
    }

    pub fn running(&self) -> bool {
        self.view.running()
    }

    /// 切换界面语言（`/locale` 写回成功之后）：之后画的每一帧都用新语言。
    pub fn set_lang(&mut self, lang: Lang) {
        self.lang = lang;
        self.strings = super::i18n::strings(lang);
    }

    /// 发送输入时的投递方式：运行中插话（enter 与 ctrl+x enter 相同）；空闲时内核立即开始回合
    /// （docs/specs/rust-cli/tui.md §5.1）。
    pub fn delivery(&self) -> Delivery {
        if self.running() {
            Delivery::Steer
        } else {
            Delivery::Queue
        }
    }

    /// 是否需要定时重绘（运行中的动画、拖选到边缘、复制提示、临时提示）。
    pub fn animating(&self) -> bool {
        let dragging_at_edge = self.selection.is_some_and(|s| s.dragging && s.edge != 0);
        let copied = self
            .copied
            .as_ref()
            .is_some_and(|(notice, at)| at.elapsed() < notice.duration);
        // 背景标志只在点击后的约 2 秒里按动画节拍重画，停住后不再定时重画（tui.md §7.3）。
        self.running()
            || self.creating
            || self.notice_text().is_some()
            || dragging_at_edge
            || copied
            || self.logo.animating(Instant::now())
    }

    /// 新会话的背景标志画不画（tui.md §7.3）：还没有会话（也不在建）、输入框为空、没有打开视图、不在查看子代理。
    pub fn shows_logo(&self) -> bool {
        self.session.is_none()
            && !self.creating
            && self.held.is_empty()
            && self.composer.is_empty()
            && self.overlay.is_none()
            && self.viewing.is_none()
            && self.pending().is_none()
    }

    /// 要显示的待处理交互：当前会话的在前，之后是旧会话的询问（按到达顺序，agents.md §4.1.2）。
    pub fn pending(&self) -> Option<Pending<'_>> {
        pending_of(&self.view, &self.detached)
    }

    /// 这个交互是旧会话的询问时，那个会话的标题。
    pub fn detached_title(&self, id: &rust_cli_kernel_api::ids::InteractionId) -> Option<&str> {
        self.detached
            .iter()
            .find(|ask| ask.interaction == *id)
            .map(|ask| ask.title.as_str())
    }

    /// 旧会话询问的变化（工作线 F72）。
    pub fn detached_changed(&mut self, event: rust_cli_kernel_api::orchestration::DetachedEvent) {
        use rust_cli_kernel_api::orchestration::DetachedEvent;
        match event {
            DetachedEvent::Asked { ask } => self.detached.push(*ask),
            DetachedEvent::Resolved { interaction } => {
                self.detached.retain(|ask| ask.interaction != interaction);
            }
        }
    }

    /// 视图变化后调用：交互换了就重置选择状态，交互结束就关闭视图。
    pub fn sync_interaction(&mut self, now: Instant) {
        let Some((id, request)) = self.pending() else {
            self.interaction = None;
            return;
        };
        if self.interaction.as_ref().is_none_or(|ui| ui.id != *id) {
            let mut ui = Interaction::new(id.clone(), request, now);
            ui.mode = self.view.mode();
            ui.workspace = self.workspace().to_owned();
            // 远端执行 F98（remote-exec.md §25.4.3，Docker 试用 L4、L5）：远端的调用，“不再询问”写那台机器的工作目录（以前
            // 写本机的工作区）；MCP 工具的机器来自服务器的绑定（以前询问框不写机器）。
            if let rust_cli_kernel_api::orchestration::InteractionRequest::Permission {
                tool,
                input,
                machine,
                ..
            } = request
            {
                if machine.is_none() {
                    ui.bound_machine = super::machines::remote_of(tool, input, &self.mcp_machines)
                        .map(str::to_owned);
                }
                if let Some(remote) = machine.as_deref().or(ui.bound_machine.as_deref()) {
                    ui.workspace = self
                        .remote_root(remote)
                        .unwrap_or_else(|| remote.to_owned());
                }
            }
            ui.origin = self.detached_title(id).map(str::to_owned);
            self.interaction = Some(ui);
        }
    }

    /// 把空闲计时的倒计时交给提问框显示（工作线 F72）。
    pub fn show_countdown(&mut self) {
        let countdown = self.afk.countdown();
        if let Some(ui) = self.interaction.as_mut() {
            ui.countdown = countdown;
        }
    }

    /// 在 transcript 末尾插入一个本地 cell。
    pub fn push_local(&mut self, local: Local) {
        let anchor = self.view.rows().last().map(|row| row.id);
        self.local.push(LocalCell { anchor, local });
        self.scroll = 0;
    }

    pub fn info(&mut self, text: impl Into<String>) {
        self.push_local(Local::Info(text.into()));
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.push_local(Local::Error(text.into()));
    }

    /// footer 与启动头写的模型名。
    pub fn model_label(&self) -> Option<String> {
        let model = &self.view.selection().model;
        (!model.is_empty()).then(|| match &self.view.selection().reasoning {
            Some(reasoning) => format!("{model} {reasoning}"),
            None => model.clone(),
        })
    }

    /// 内容变长时保持用户正在看的位置（已上滚时）。
    pub fn content_resized(&mut self, lines: usize) {
        if self.scroll > 0 && lines > self.transcript_lines {
            self.scroll += lines - self.transcript_lines;
            self.new_activity = true;
        }
        self.transcript_lines = lines;
        self.scroll = self.scroll.min(lines.saturating_sub(self.viewport));
        if self.scroll == 0 {
            self.new_activity = false;
        }
    }

    pub fn scroll_by(&mut self, lines: isize) -> Option<Action> {
        let max = self.transcript_lines.saturating_sub(self.viewport);
        let next = (self.scroll as isize + lines).clamp(0, max as isize) as usize;
        self.scroll = next;
        if next == 0 {
            self.new_activity = false;
        }
        None
    }

    /// 离开当前会话（`/clear`）：界面状态回到新会话，模型与模式沿用。
    /// 当前工作区的根（会话的，或新会话的草稿）。
    pub fn workspace(&self) -> &str {
        self.view.workspace().root.as_str()
    }

    /// 显示用的工作区路径（家目录写成 `~`）。
    pub fn workspace_label(&self) -> String {
        label(self.workspace(), self.home.as_deref())
    }

    /// 离开会话（`/clear`、切换会话）：模型、模式与工作区留作新会话的草稿（`/clear` 留在原来的工作目录：
    /// EnterWorktree 与 `/cd` 都改它）；`@` 补全的索引属于旧工作区，丢掉。
    pub fn reset(&mut self) {
        self.view = Transcript::new(
            self.view.selection().clone(),
            self.view.execution(),
            self.view.workspace().clone(),
        );
        self.composer.files = Default::default();
        self.session = None;
        self.interaction = None;
        self.local.clear();
        self.expanded_cells.clear();
        self.scroll = 0;
        self.new_activity = false;
        self.selection = None;
        self.screen_selection = None;
        self.tasks = super::tasks::TaskPanel::default();
        self.background.clear();
        // 离开的会话的请求统计不再显示，在途的回执作废（F27）。
        self.request_stats.clear();
        self.agents = super::agents::AgentList::default();
        self.viewing = None;
        self.focus = None;
        // 切到完全访问的确认框属于离开的会话：作废，不发命令（tui.md §7.8；App 在目标换代时同样作废）。
        if matches!(self.overlay, Some(Overlay::YoloConfirm(_))) {
            self.overlay = None;
        }
    }
}

/// 显示路径：家目录写成 `~`，两种分隔符都认。
pub(crate) fn label(path: &str, home: Option<&str>) -> String {
    match home {
        Some(home) if path == home => "~".to_owned(),
        Some(home) => match path.strip_prefix(home) {
            Some(rest) if rest.starts_with(['/', '\\']) => format!("~{rest}"),
            _ => path.to_owned(),
        },
        None => path.to_owned(),
    }
}
