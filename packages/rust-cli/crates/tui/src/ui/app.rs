// 改写自 apps/zcode-host/crates/tui/src/ui/app.rs（feat/rs 79b6691c55）：会话内容换成 `view::Transcript`，
// Action 换成内核会话 API 的命令（docs/specs/rust-cli/tui.md §2、§4）。
//! 界面状态：会话内容只来自视图（`view::Transcript`）；这里只有草稿、视图、滚动、展开等界面状态，
//! 以及插在 transcript 里的本地 cell（提示、错误）。不做 IO：按键产生 [`Action`]，由 runner 执行。
use std::time::Instant;

use rust_cli_kernel_api::session_api::Delivery;

use super::app_state::{KeyState, Pointer, Port, Snapshots};
use super::composer::Composer;
use super::i18n::{Lang, Strings};
use super::ink::Ink;
use super::interaction::Interaction;
use super::probe::Colors;
use crate::view::Transcript;

// 动作的类型与滚动的方法在子模块里（文件不过 800 行）；调用方照旧写 `app::Action`。
mod action;
mod scroll;

pub use action::Action;

/// 插在 transcript 里的本地 cell（不属于会话事实）。
#[derive(Debug, Clone, PartialEq)]
pub enum Local {
    Info(String),
    Error(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct LocalCell {
    /// 插入时 transcript 的最后一行；cell 显示在它之后。
    pub anchor: Option<u64>,
    pub local: Local,
}

/// 输入框上方的话（Puffin `problems` 的 `notice` 与 `quiet`，各自一行，先失败的后安静的）：本地命令的结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Said {
    /// 一时的失败：` ✗ ` 与 fg2 的一句。
    pub failed: Option<String>,
    /// 安静的话：fg3，可以几行（复制的途径说明接在复制的结果下面）。
    pub quiet: Option<String>,
}

/// 替换输入框或盖在板上的视图；类型与它属于哪一层在 `overlay.rs`。
pub use super::overlay::Overlay;

/// 恢复的会话的标题行要的：它开始的时刻（epoch ms）与那一刻本地时间相对 UTC 的分钟数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resumed {
    pub created_ms: u64,
    pub utc_offset: i32,
}

pub struct App {
    /// 墨色（tui-kit.md §3.4）：画面只经它取色。终端的颜色事实与 `theme.json` 的选择解出。
    pub ink: Ink,
    pub lang: Lang,
    pub strings: &'static Strings,
    /// 会话的视图；没有会话时是新会话的草稿（模型与模式）。
    pub view: Transcript,
    /// 当前会话的 ID（`/status` 与退出提示用）；runner 打开会话后写入。
    pub session: Option<String>,
    /// 正在创建会话（第一次发送）。
    pub creating: bool,
    /// 入口的缺省模型还没定下（在后台取模型目录，performance.md §12.3）：留下的消息说等 agent，不说等接上模型。
    pub model_pending: bool,
    /// 创建期间的发送（tui.md §4.1）：会话打开、第一条发出后按顺序发给内核；显示在排队的输入后面。
    pub held: Vec<rust_cli_kernel_api::session_api::Command>,
    /// 界面层完成的本地命令的记录（`/plan` 等，修复线 L-A，tui-manage.md §11）：runner 每轮取走。
    pub records: Vec<rust_cli_kernel_api::user_command::LocalCommandRecord>,
    /// 运行中的 `/clear`、`/resume`（修复线 F38，tui.md §6；排进队列、不中断）：（显示的文字，动作），回合结束、
    /// 内核队列空了之后按顺序执行；显示在排队的输入后面。
    pub after_turn: Vec<(String, Action)>,
    /// 正在取回的排队消息（等内核的回执）：回执之前 ⌫ 不再请求（Puffin 的 `taking`）。
    pub taking: Option<rust_cli_kernel_api::ids::CommandId>,
    pub composer: Composer,
    /// skill 清单第一次到之前提交、当时判定不了的命令（tui.md §6）：清单到了（`commands_arrived`）再提交一次。
    pub held_command: bool,
    pub overlay: Option<Overlay>,
    /// 当前待处理交互的选择状态；交互本身在视图里（旧会话的询问在 `detached` 里）。
    pub interaction: Option<Interaction>,
    /// 已回答、等离开的请求与每个请求第一次出现的时刻（Puffin `answered`、`ask_seen`，interaction.rs）。
    pub asks: super::interaction::Asks,
    /// 父会话关闭之后旧会话的询问（按到达顺序，agents.md §4.1.2，工作线 F72）：内核的 `detached_asks` 的投影。
    pub detached: Vec<rust_cli_kernel_api::orchestration::DetachedAsk>,
    /// 同伴消息批准框的焦点（0 拒绝，1 投递）；扣住的消息本身在视图里（cross-session.md §2.6）。
    pub peer_choice: usize,
    pub local: Vec<LocalCell>,
    /// 显示的 transcript 里的读位置（滚动、单独展开、搜索）。
    pub port: Port,
    /// 鼠标与选区。
    pub pointer: Pointer,
    /// 按键的状态。
    pub keys: KeyState,
    /// 当前会话的快照（任务、后台任务、请求统计、子代理）。
    pub snapshots: Snapshots,
    /// 上次排版的 transcript 总行数与可视高度。
    pub transcript_lines: usize,
    pub viewport: usize,
    /// 上一帧画出的顶行与跟随末尾时正文的行数（`draw` 记下）：滚动从画出的帧算（Puffin `scroll`，docs/puffin-tui.md §4.4）。
    pub shown_top: usize,
    pub follow_rows: usize,
    /// 按了 esc 停下的回合（它的回合头的行 id）：回合真正结束之前笔尖已经消失、字不再落定（Puffin `stop_pending`）。
    pub stop_pending: Option<u64>,
    /// ctrl+o：transcript 按详细的样子画（Puffin `details_open`：一段工作展开、思考显示全文、长的消息不剪）。以前 ctrl+o
    /// 打开占满屏幕的详细视图。
    pub details_open: bool,
    /// footer 的临时提示与等待第二次按下的按键。
    pub notice: Option<super::notice::Notice>,
    /// 最后一个会话事件到达的时间；状态行据此判断卡住。
    pub last_frame: Instant,
    /// 家目录（显示路径时写成 `~`）；工作区本身只在 `view` 里（R1-3）。
    pub home: Option<String>,
    pub version: String,
    pub started: Instant,
    /// 新会话的背景标志与它的按压（tui.md §7.3）。
    pub mark: super::logo::Mark,
    /// 输入框文字区的宽度（上下移动与折行用）；每帧由绘制更新。
    pub composer_width: usize,
    /// rewind 选择器的条目（打开时由 runner 填入）。
    pub rewind: Vec<super::rewind::Entry>,
    /// 挑选回退的点（docs/puffin-tui.md §4.2「回退（/rewind）」，`rewind_pick.rs`）：不挑选时为 None。
    pub picking: Option<super::rewind_pick::Picking>,
    /// esc 连按两次要开始挑选：回退的点到了（`open_rewind`）就在最后一个点上挑，不开列表。
    pub picking_wanted: bool,
    /// `/memory` 列出的文件（入口给出，tui-input.md §4.2）。
    pub memory_files: Vec<crate::services::MemoryFile>,
    /// 终端通知的计时（tui-manage.md §5）。
    pub notify: super::notify::Notify,
    /// 程序状态（OSC 7501）的界面状态（tui-manage.md §5.4）。
    pub program_status: super::program_status::ProgramStatus,
    /// 提问框的空闲计时（tools-more.md §1.1，工作线 F72）。
    pub afk: super::afk::Afk,
    pub(super) dock: super::agents_dock::DockSeen, // 见过在跑的顶层后台子代理（agents_dock.rs）
    pub(super) line_seen: super::task_line::LineSeen, // 任务行出现与记号变化的时刻（task_line.rs）
    /// 任务卡上次看的种类（tui-tasks.md §3.1）：再打开时落在这里。
    pub task_kind: Option<super::task_card::TaskKind>,
    /// 正在查看的子代理（只读）。
    pub viewing: Option<super::agents::Viewing>,
    /// npm 上有更新的版本（docs/specs/rust-cli/update.md §7）：欢迎块的版本行尾说 `X is out · zcode update`（Puffin
    /// `newer_release`）。以前是会话头下面的一块提示，切换会话时搬到新视图的顶上。
    pub newer_release: Option<String>,
    /// 会话现在的目标（kit 的形状，cards.rs）：状态栏模式后面的记号与字（kit `goal::status_parts`）与目标的卡说的。runner
    /// 在接上会话、目标的检查到达与 `/goal` 时从会话取（Puffin 投影的 `goal`）；`goal_met_seen` 是发送时达成的目标的键
    /// （Puffin `goal_met_seen`：达成的字随下一次发送消失）。
    pub session_goal: Option<super::cards::SessionGoal>,
    /// `/usage` 读到的计划、额度与历史（usage_plans.rs）。
    pub usage_plans: super::usage_plans::UsageUi,
    /// 上下文的计数：`/stats` 卡的组成与上下文的页唯一的数据源（context_count.rs，tui-panels.md §4.1）。
    pub context_count: super::context_count::ContextCount,
    /// 统计的页 Session 标签的事实（MCP、hooks、账号、分支；session_page.rs，tui-panels.md §4.5）：还没问到时 None。
    pub session_facts: Option<super::session_page::SessionFacts>,
    /// 本地时间相对 UTC 的分钟数（按时刻）：`/usage` 的日历（runner 按 `Options.clock` 装上）。
    pub zone: rust_cli_tui_kit::app::widgets::usage::UtcOffset,
    /// 上一帧的页（`page_frame.rs`）：正文的行数与页的行数；页的键按它滚动（Puffin 按键时取同一几何）。
    pub page_rows: (usize, usize),
    /// 驾驶舱记着的分组（启动时读 `puffin-tui/cockpit.json`，⇥ 时写）。
    pub cockpit_by_folder: bool,
    /// 别的会话里有消息的组（不是 Rest 的；runner 在页关着时按监听重读，tui-cockpit.md §3.4）：槽的 ← 提示按它数，画的时候
    /// 才去掉现在的会话。
    pub cockpit_news: Vec<(String, super::cockpit::Kind)>,
    /// 这个进程所在的 tmux（驾驶舱的“去那里”比较它，tui-cockpit.md §3.3）。
    pub tmux_socket: Option<String>,
    /// 上一帧的屏宽与墙上时间：复制页的键按它重排这一页（`page_copy.rs`，同 `page_rows` 取上一帧的几何）。
    pub page_at: (usize, u64),
    /// 上一帧设置页的页（不含输入框与状态栏）：设置页的 ⌃y 复制它（`page_copy.rs`）。
    pub settings_shown: Vec<rust_cli_tui_kit::render::StyledLine>,
    /// 刚复制了页：页的槽里是 `Copied the page`，到下一个键（同 Puffin 门道的 `Copied the link`）。
    pub page_copied: Option<super::session_page::Copied>,
    /// ^T 时会话里没有任务：卡的位置说一句，到下一个键（tui-tasks.md §3.1，`task_card::open_tasks`）。
    pub no_tasks: bool,
    pub goal_met_seen: Option<String>,
    /// `!` 命令的 shell 的名字（`Services::shell_name`）与这次运行里有没有说过 shell 模式做什么（Puffin `shell_noted`）。
    pub shell_name: String,
    /// 在远程 shell 里（`Services::remote_shell`，Puffin `place.remote`）：门道的登录链接整段折出。
    pub remote_shell: bool,
    pub shell_noted: bool,
    /// 恢复的会话（`/resume`）：transcript 的顶上是它的标题与开始的时刻，不是欢迎块（Puffin `header`）。
    pub resumed: Option<Resumed>,
    /// `/resume` 列表正在打开的会话上次用的时刻（毫秒，Puffin `switching` 的 `updated_at`）：到了时说 `Last active …`。
    pub opening: Option<u64>,
    /// 门道（`doorway.rs`）与它关上时说的接上了什么（欢迎块的第 4 行，这次运行一直在）。
    pub door: Option<super::doorway::Door>,
    pub connected: Option<super::doorway::Connected>,
    /// 机器服务的最新快照（远端执行 R4，remote-exec.md §21.2；runner 在服务的变化通知之后重取）：状态行与 `/machines`。
    pub machines: Vec<crate::machines::MachineRow>,
    /// 连接中的机器从什么时候起在连（Unix 毫秒，按名字）：坞行写连了多久（Puffin `kept.connecting` 的 `since_ms`）。
    pub machines_since: std::collections::HashMap<String, u64>,
    /// 输入框上方的一句（[`Said`]）。
    pub said: Said,
    /// 开着的设置页（tui-kit.md §4.2「设置」）：正文换成它，输入框是它的搜索。
    pub settings: Option<super::settings::Settings>,
    /// 槽说的话与它从什么时候起这么说：槽变了时从那一刻淡入（Puffin `slot_at`，docs/puffin-tui.md §4.2「按键提示（槽）」）。
    pub slot_seen: Option<(String, Instant)>,
    /// 输入框里在淡入的 `!`、粘贴的帽与预览（input_motion.rs）。
    pub bottom_motion: super::input_motion::Motion,
    /// 状态栏的时钟与现场的速度（status_motion.rs）。
    pub status_motion: super::status_motion::StatusMotion,
    /// esc 关上了草稿 `?` 的按键表（草稿再变成打出的 `?` 时打开，Puffin `keys_closed`；draft_layer.rs）。
    pub keys_closed: bool,
    /// 已经请 runner 读模型目录、回答还没到（draft_layer.rs `request_models`）。
    pub models_opening: bool,
    /// 入口注入了机器服务（没有时 `/machines` 提示不可用）。
    pub machines_enabled: bool,
    /// 绑定到远端机器的 MCP 服务器（规范化的服务器名，机器名；R5，remote-exec.md §22.8）：runner 启动时从服务取。
    pub mcp_machines: Vec<(String, String)>,
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
        colors: Colors,
        lang: Lang,
        view: Transcript,
        home: Option<String>,
        version: String,
    ) -> Self {
        Self {
            ink: colors.ink(),
            lang,
            strings: super::i18n::strings(lang),
            view,
            session: None,
            creating: false,
            model_pending: false,
            held: Vec::new(),
            records: Vec::new(),
            after_turn: Vec::new(),
            taking: None,
            composer: Composer::default(),
            overlay: None,
            interaction: None,
            asks: Default::default(),
            detached: Vec::new(),
            peer_choice: 0,
            local: Vec::new(),
            port: Port::default(),
            pointer: Pointer::default(),
            keys: KeyState::default(),
            snapshots: Snapshots::default(),
            transcript_lines: 0,
            viewport: 0,
            shown_top: 0,
            follow_rows: 0,
            stop_pending: None,
            details_open: false,
            notice: None,
            last_frame: Instant::now(),
            home,
            version,
            started: Instant::now(),
            mark: Default::default(),
            composer_width: super::layout::DEFAULT_WIDTH,
            rewind: Vec::new(),
            picking: None,
            picking_wanted: false,
            held_command: false,
            memory_files: Vec::new(),
            notify: super::notify::Notify::default(),
            program_status: Default::default(),
            afk: super::afk::Afk::default(),
            dock: super::agents_dock::DockSeen::default(),
            line_seen: Default::default(),
            task_kind: None,
            viewing: None,
            newer_release: None,
            session_goal: None,
            usage_plans: Default::default(),
            context_count: Default::default(),
            session_facts: None,
            zone: std::sync::Arc::new(|_| 0),
            page_rows: (1, 0),
            cockpit_by_folder: false,
            cockpit_news: Vec::new(),
            tmux_socket: None,
            page_at: (super::layout::DEFAULT_WIDTH, 0),
            settings_shown: Vec::new(),
            page_copied: None,
            no_tasks: false,
            goal_met_seen: None,
            shell_name: "sh".to_owned(),
            remote_shell: false,
            shell_noted: false,
            resumed: None,
            opening: None,
            door: None,
            connected: None,
            machines: Vec::new(),
            machines_since: std::collections::HashMap::new(),
            said: Said::default(),
            settings: None,
            slot_seen: None,
            bottom_motion: Default::default(),
            status_motion: Default::default(),
            keys_closed: false,
            models_opening: false,
            machines_enabled: false,
            mcp_machines: Vec::new(),
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

    /// 新会话的背景标志画不画（tui.md §7.3）：还没有会话（不在建会话，没有留下的输入）、不在查看子代理、不是恢复的会话、
    /// 门道关着。打字时还在；发出第一条消息就没有；开着的层只是盖住它。
    pub fn shows_mark(&self) -> bool {
        self.session.is_none()
            && !self.creating
            && self.held.is_empty()
            && self.viewing.is_none()
            && self.resumed.is_none()
            && self.door.is_none()
    }

    /// 是否需要定时重绘（运行中的动画、拖选到边缘、复制提示、临时提示）。`now` 是这一帧的动画时钟（同 `draw` 的 `now`）。
    pub fn animating(&self, now: Instant) -> bool {
        let dragging_at_edge = self
            .pointer
            .selection
            .is_some_and(|s| s.dragging && s.edge != 0);
        let copied = self
            .pointer
            .copied
            .as_ref()
            .is_some_and(|(notice, at)| at.elapsed() < notice.duration);
        // 查看在跑的子代理时状态行的
        // spinner 是它的（F112），主会话空闲时也按动画节拍重画。
        let viewed_running = self.viewed_entry().is_some_and(|entry| {
            entry.status == rust_cli_kernel_api::panels::SubagentStatus::Running
        });
        self.running()
            || viewed_running
            // 背景标志只在画着、按压或亮度还没停下时重画（tui.md §7.3）。
            || (self.pointer.hits.mark.is_some() && self.mark.moving(now))
            || self.creating
            || self.notice_text().is_some()
            || dragging_at_edge
            || copied
            || self.slot_fading()
            || self.settings_moving()
            || self.cards_fading()
            || self.bottom_motion.moving()
            || self.status_motion.moving()
            // 缺陷修复（twin-harness，2026-10-31）：以前读 `Instant::now()`，孪生里重不重画随机器快慢；改按这一帧的时钟。
            || self.asks_moving(now)
            || self.line_moving(now)
            // `@` 与 `$` 面板的淡入（at_view.rs、skill_view.rs）：以前 `@` 面板不在这里，真终端里淡入停在第一帧。
            || self.at_moving(self.status_motion.tick())
            || self.skills_moving(self.status_motion.tick())
            // 驾驶舱里换组的行与窗格的淡入（cockpit_view.rs）。
            || self.cockpit_fading(now)
    }

    /// 槽在淡入（kit `fade::TICKS` 个 tick）：按动画节拍重画到它落定。
    pub fn slot_fading(&self) -> bool {
        self.slot_seen.as_ref().is_some_and(|(_, at)| {
            at.elapsed().as_millis()
                < u128::from(rust_cli_tui_kit::app::widgets::playhead::TICK_MS)
                    * rust_cli_tui_kit::app::widgets::fade::TICKS as u128
        })
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

    /// 视图变化后调用：交互换了就重置选择状态，交互结束就关闭视图（`interaction_keys.rs` 的 `sync_asks`）。
    pub fn sync_interaction(&mut self, now: Instant) {
        self.sync_asks(now);
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
        self.port.scroll = 0;
    }

    /// 本地命令的结果。一行的写在输入框上方安静的一行（Puffin `problems` 的 `quiet`，tui-kit.md §4.0「提示与安静的一行」），
    /// 下一次发送、下一个命令或 `/clear` 时清掉；多行的（表格、`/status`）照旧是 transcript 里的本地块。
    /// TODO(kit-k3)：kit 的 notices 落地后改用它。
    pub fn info(&mut self, text: impl Into<String>) {
        let text = text.into();
        if text.trim_end().contains('\n') {
            self.push_local(Local::Info(text));
        } else if !self.settings_say(Ok(text.trim_end())) {
            // 设置开着时写在页的输入框上方（Puffin `machines_say`），关着时是安静的一行。
            self.said.quiet = Some(text.trim_end().to_owned());
        }
    }

    /// 失败的结果：一行的是输入框上方的 ` ✗ ` 与 fg2 的一句（Puffin 一时的失败），多行的照旧进 transcript。
    pub fn error(&mut self, text: impl Into<String>) {
        let text = text.into();
        if text.trim_end().contains('\n') {
            self.push_local(Local::Error(text));
        } else if !self.settings_say(Err(text.trim_end())) {
            self.said.failed = Some(text.trim_end().to_owned());
        }
    }

    /// 现在看的是谁的视图（修复线 F112，tui-panels.md §1.7）：查看子代理时是它的只读视图，否则是主会话的。footer 的模型、
    /// 档位、TPS、ctx、上下文提示与权限模式，以及 transcript 的工作区都从这里取，不把子代理的值复制进主会话的状态。
    pub fn shown(&self) -> &Transcript {
        self.viewing
            .as_ref()
            .map_or(&self.view, |viewing| &viewing.view)
    }

    /// footer 的命中率查谁（F112）：查看时是被查看的子代理的子会话（取自列表快照的条目），否则是主会话；没有会话、或
    /// 被查看的条目不在快照里时不查。
    pub fn stats_session(&self) -> Option<rust_cli_kernel_api::ids::SessionId> {
        match &self.viewing {
            Some(_) => self.viewed_entry().map(|entry| entry.child.clone()),
            None => self
                .session
                .as_deref()
                .map(rust_cli_kernel_api::ids::SessionId::new),
        }
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

    /// 显示的 transcript 换了（离开会话、开始查看子代理、换一个、返回主会话）：读位置（滚动、单独展开、搜索）与两种选区都
    /// 属于原来那份。
    ///
    /// 缺陷修复（架构整理 U3）：换显示的 transcript 原来在 runner 里另有一份手写的重置列表，漏了全屏视图的选区：异步的
    /// 查看切换（被查看的子代理离开快照、回到主会话）落在详细视图打开时，屏幕坐标的选区留在换了内容的画面上。现在两处
    /// 都调这里。
    pub fn shown_changed(&mut self) {
        self.port = super::app_state::Port::default();
        self.pointer.clear_selection();
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
        self.asks = Default::default();
        self.local.clear();
        // 读位置（滚动、单独展开、搜索 F114）与两种选区属于离开的那份 transcript。
        self.shown_changed();
        self.resumed = None;
        self.stop_pending = None;
        // 离开的会话的任务、后台任务与子代理不再显示；请求统计的在途回执作废（F27）。
        self.snapshots.clear();
        // 目标属于离开的会话（cards.rs）。
        self.session_goal = None;
        // 失败回合的上限按这个视图的行 id 记（usage_back.rs）。
        self.usage_plans.walls.clear();
        self.usage_plans.walls_asking.clear();
        self.goal_met_seen = None;
        self.viewing = None;
        // 任务卡属于离开的会话（tui-tasks.md §3.1）。
        if matches!(self.overlay, Some(Overlay::Tasks(_))) {
            self.overlay = None;
        }
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
