//! 事件循环（docs/specs/rust-cli/tui.md §4）：终端输入、内核事件、命令的回执与动画节拍在一个循环里按序
//! 处理。只经内核会话 API（`Kernel`、`SessionHandle`、`Command`、`KernelEvent`）驱动会话；命令的回执
//! 用 `FuturesUnordered` 在循环里轮询，不另起任务。
mod actions;
mod at_files;
mod auto_compact;
mod cockpit;
mod fork_goal;
mod input;
mod latest;
mod lifecycle;
mod machines;
mod manage;
mod model_default;
mod outbox;
mod panels;
mod projects;
mod records;
mod restore;
mod services;
mod session;
mod settings;
mod usage;

pub use lifecycle::Startup;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use crossterm::event::Event;
use futures_util::FutureExt;
use futures_util::stream::{self, BoxStream, FuturesUnordered, Stream, StreamExt};
use rust_cli_kernel_api::clock::{Clock, Ids};
use rust_cli_kernel_api::default_model::DefaultModel;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::provider::ModelDirectory;
use rust_cli_kernel_api::session_api::{
    Command, CommandEnvelope, EndReason, Kernel, SequencedEvent, SessionHandle, StartSource,
};
use rust_cli_kernel_api::session_follow::follow;
use rust_cli_tui_kit::render::ColorDepth;
use rust_cli_tui_kit::render::renderer::Renderer;
use rust_cli_tui_kit::theme::scheme::ThemeChoice;

use crate::ui::app::App;
use crate::ui::frame::Frame;
use crate::ui::probe::{ColorLevel, Colors};
use crate::ui::transcript::Cache;
use crate::view::Transcript;
use session::Done;

/// 入口给 TUI 的参数。环境变量只有入口读，这里是它给的快照（tui.md §2）。
#[derive(Clone)]
pub struct Options {
    /// 启动时的工作区：第一个新会话的草稿。之后的工作区只从会话的快照与 `WorkspaceChanged` 取（R1-3），
    /// runner 不再读它。
    pub workspace: WorkspaceRef,
    /// 家目录（显示路径时写成 `~`）。
    pub home: Option<String>,
    /// 新会话的缺省模型（工作线 F140，performance.md §12.3）：入口在后台取模型目录时还没定下，第一帧不等它；
    /// 定下时草稿还没有模型就用它（[`Done::DefaultModel`]）。
    pub selection: DefaultModel,
    /// 新会话的权限模式。
    pub mode: PermissionMode,
    /// 进程启动的时刻：第一帧画完时记 `startup.ready`（logging.md §4）。
    pub started: Instant,
    pub version: String,
    /// `TERM`、`COLORTERM`、`LANG`、`LC_ALL`、`LC_MESSAGES`、`TERM_PROGRAM` 等。
    pub vars: BTreeMap<String, String>,
    /// 模型目录（`/model`）；没有时只能用 `/model 名称` 以外的方式换模型。
    pub models: Option<Arc<dyn ModelDirectory>>,
    /// 入口载入的本工作区输入历史（从旧到新）与追加入口（tui.md「输入历史」）；TUI 不做文件 IO。
    pub history: Vec<String>,
    pub recorder: Option<Arc<dyn crate::services::HistoryRecorder>>,
    /// 启动时打开的会话（`--resume` / `-c`）；`fork` 为真时打开后先 fork（`--fork-session`）。
    pub startup: Startup,
    pub fork: bool,
    /// 入口给的配置值（界面语言、主题、`/memory` 的文件等，启动时定了）。
    pub ui: crate::services::UiConfig,
    /// 入口注入的服务（tui.md §7.1、§7.2，tui-input.md §3、§4）。
    pub services: crate::services::Services,
    /// 入口收到退出信号时取消（session-lifecycle.md §3.5）：中止在跑的回合，等它结束后离开。
    pub stop: tokio_util::sync::CancellationToken,
    /// 入口注入的时钟与 ID（与内核同一份，README §4）：输入历史与界面上的时间、命令 ID（F7，审查 R2-11：以前
    /// TUI 直接读系统时间、直接生成 uuid）。
    pub clock: Arc<dyn Clock>,
    pub ids: Arc<dyn Ids>,
    /// 运行的平台（修复线 F65）：按键的含义（AltGr、alt 码、paste burst，tui-input.md §9）与颜色级别（tui.md §7.4）。
    pub os: crate::os::Os,
}

/// 退出时的结果：当前会话（入口打印恢复提示）与还没跑的 SessionEnd。
///
/// 退出的顺序（session-lifecycle.md §3.5）：先恢复终端、打印恢复提示，再跑 SessionEnd。终端在
/// [`run`] 返回前恢复，所以事件循环不关会话，交给入口在打印恢复提示之后调用 [`Ending::run`]。
/// 缺陷修复（F14）：以前事件循环自己先关会话，SessionEnd 跑在恢复终端之前。
pub struct Exit {
    pub session: Option<String>,
    pub ending: Ending,
}

/// 离开时的 SessionEnd（`prompt_input_exit`；退出信号是 `other`）与关闭会话。持有内核：内核释放时会话的 actor
/// 随之结束，SessionEnd 就跑不了了。
pub struct Ending {
    _kernel: Arc<dyn Kernel>,
    session: Option<Arc<dyn SessionHandle>>,
    reason: EndReason,
}

impl Ending {
    /// 跑 SessionEnd 并关闭会话；返回 hook 的提示（入口写到 stderr）。之后内核随它释放。
    pub async fn run(self) -> Vec<String> {
        match self.session {
            Some(session) => session.close(self.reason).await,
            None => Vec::new(),
        }
    }
}

/// 终端：尺寸、帧的字节、副作用（复制）与每帧之后的回调；真实终端写到 stdout，测试记录画面（tui-kit.md §3.3、§3.7）。
pub trait Screen {
    /// 用 OSC 52 把文字放进剪贴板（序列按终端复用器，`ui::clipboard::osc52`）。系统剪贴板命令不在这里：经入口的
    /// `Clipboard::copy_text`（tui-select.md §3.2）。
    fn copy(&mut self, text: &str);
    /// 终端的（列，行）。
    fn size(&self) -> std::io::Result<(u16, u16)>;
    /// 写出渲染器的字节（一帧，或离开时的恢复）。失败就是终端没了（F14）。
    fn write(&mut self, bytes: &str) -> std::io::Result<()>;
    /// 每画完一帧调用一次：这一帧的行（终端上的格子由它们按 `width` 列铺成，与渲染器相同）。
    fn drawn(&mut self, _frame: &Frame, _width: u16) {}
    /// 终端通知与程序状态的转义序列（tui-manage.md §5.2、§5.4）：真实终端原样写到 stdout。
    fn notify(&mut self, _sequence: &str) {}
    /// 终端分辨得出增强按键吗（修复线 F104，tui-input.md §11.1.1）：真实终端是进界面时键盘增强的查询结论
    /// （`ui::terminal::enter`）。决定立即发送的提示写哪个键；缺省否。
    fn keyboard_enhanced(&self) -> bool {
        false
    }
    /// 终端支持程序状态（OSC 7501，tui-manage.md §5.4）吗：真实终端是进界面时探测的结论（`ui::probe`）。不支持时不写任何
    /// 7501 序列；缺省否。
    fn program_status(&self) -> bool {
        false
    }
    /// 把终端交给子进程（外部编辑器，tui-input.md §4.1）：暂停读输入、离开 alternate screen 与 raw mode。
    /// 交出期间不画帧；测试里不做事。
    fn suspend(&mut self) {}
    /// 收回终端；之后整屏重画。
    fn resume(&mut self) {}
}

/// 旧的颜色级别 → 渲染器的颜色深度（tui-kit.md §3.4）：级别仍由 `ColorLevel::detect` 定（含 Windows 的版本判断）。
fn depth(level: ColorLevel) -> ColorDepth {
    match level {
        ColorLevel::TrueColor => ColorDepth::TrueColor,
        ColorLevel::Ansi256 => ColorDepth::Ansi256,
        ColorLevel::Ansi16 => ColorDepth::Ansi16,
    }
}

/// `theme.json` 的选择（与 Puffin 同一个文件、同一格式，tui-kit.md §3.4）：没有入口给的存储、没有文件或读不出时是缺省。
pub(crate) async fn theme_choice(stores: Option<&dyn crate::services::Stores>) -> ThemeChoice {
    let Some(stores) = stores else {
        return ThemeChoice::default();
    };
    match stores.read(crate::services::StoreFile::Theme).await {
        Ok(Some(text)) => ThemeChoice::decode(&text),
        _ => ThemeChoice::default(),
    }
}

/// 还没有会话、或正在重新取快照时的事件流：永远不产生事件。
fn idle() -> BoxStream<'static, SequencedEvent> {
    stream::pending().boxed()
}

/// 一次唤醒最多连续处理的就绪内核事件数（之后先画一帧）。
const READY_EVENTS: usize = 256;

struct Loop<'a> {
    kernel: Arc<dyn Kernel>,
    options: Options,
    app: App,
    cache: Cache,
    /// kit 的渲染器（tui-kit.md §3.3）：对上一帧逐格 diff，一帧一次写。
    renderer: Renderer,
    screen: &'a mut dyn Screen,
    /// 当前会话的句柄与 runner 一侧稍后再发的东西（`outbox.rs`）。
    outbox: outbox::Outbox,
    events: BoxStream<'static, SequencedEvent>,
    /// 父会话关闭之后旧会话的询问（内核进程级的入口，agents.md §4.1.2，工作线 F72）。
    detached: BoxStream<'static, rust_cli_kernel_api::orchestration::DetachedEvent>,
    pending: FuturesUnordered<futures_util::future::BoxFuture<'static, Done>>,
    dirty: bool,
    /// 最后画出的一帧的动画时钟：要不要按动画节拍再画按它算（同一帧的时刻）。
    painted: Instant,
    /// 画完下一帧后复制选区（`true` 时复制后清掉选区）。
    copy_after_draw: Option<bool>,
    /// 下一个新建会话的 SessionStart 来源（`/clear` 之后是 clear）。
    next_start: StartSource,
    /// 服务操作的在途状态（界面语言的写回）。
    services: services::ServiceState,
    /// 终端交给了外部编辑器（tui-input.md §4.1）：不画帧，回来后整屏重画。
    suspended: bool,
    /// `theme.json` 的写入：同时只有一次在途，在途期间又选的只留最后一个（tui-kit.md §3.4）。
    theme: latest::Latest<ThemeChoice>,
    /// L30 面板的查询状态（子代理的变化通知与请求序号，tui-panels.md §0；修复线 F34 起子代理列表与后台任务共用一个
    /// 请求序号）。
    panels: panels::Panels,
    /// ← 项目页与 `/resume` 页的查询状态（修复线 F29）。
    projects: projects::Projects,
    /// 收到了退出信号：在等回合结束；不再画帧（SIGHUP 时终端已经没了），离开时 SessionEnd 的原因是 `other`。
    stopping: bool,
    /// esc / ctrl+c 发过中断（user-cancel；运行中的中断与空闲路径的取消唤醒都算，修复线 F45）：回合结束时据此
    /// 判断要不要撤回（restore.rs，修复线 F38）。
    user_cancel: bool,
    /// 机器服务的变化通知（远端执行 R4，remote-exec.md §21.2）：出项时重取快照；没有服务时永远不出项。
    machine_changes: BoxStream<'static, ()>,
    /// 驾驶舱的部分（tui-cockpit.md §1）：监听只在页开着时有。
    cockpit: cockpit::CockpitState,
    /// 第一帧还没画完：画完时记 `startup.ready` 并清掉（performance.md §12.2）。
    first_frame: Option<Instant>,
}

/// 跑事件循环直到退出；返回退出时的会话与还没跑的 SessionEnd（[`Exit`]）。真实终端与测试（记录画面的 [`Screen`]）走同一个
/// 循环。
pub async fn drive(
    kernel: Arc<dyn Kernel>,
    options: Options,
    colors: Colors,
    screen: &mut dyn Screen,
    mut input: impl Stream<Item = Event> + Unpin,
) -> Result<Exit> {
    let preference = options.ui.locale.map(crate::services::UiLocale::as_str);
    let lang = crate::ui::i18n::resolve(preference, &options.vars);
    // `theme.json` 的选择（tui-kit.md §3.4）：第一帧之前读一次；没有、读不出时用缺省（Terminal）。
    let choice = theme_choice(options.services.stores.as_deref()).await;
    let mut app = App::new(
        colors,
        lang,
        Transcript::new(
            options.selection.current(),
            options.mode,
            options.workspace.clone(),
        ),
        options.home.clone(),
        options.version.clone(),
    );
    app.ink =
        crate::ui::ink::Ink::chosen(colors.level, colors.foreground, colors.background, choice);
    // 滚轮每格的行数按终端（tui-select.md §4.2）；原来按终端决定要不要松开即复制，F25 起所有终端都复制。
    app.pointer.wheel = crate::ui::wheel::WheelLines::new(&options.vars);
    app.set_os(options.os);
    // 终端分辨得出 ctrl+enter 吗（修复线 F104）：立即发送的提示按它选键。
    app.keys.keyboard_enhanced = screen.keyboard_enhanced();
    app.composer.load_history(options.history.clone());
    app.memory_files = options.ui.memory_files.clone();
    app.afk = crate::ui::afk::Afk::new(options.ui.question_timeout);
    // npm 上有更新的版本（docs/specs/rust-cli/update.md §7，F48）：欢迎块的版本行尾说它（Puffin `newer_release`）。
    app.newer_release = options.ui.update.as_ref().map(|info| info.latest.clone());
    // `/usage` 的日历按本地时间（usage_plans.rs）。
    let clock = options.clock.clone();
    app.zone = Arc::new(move |at: f64| clock.utc_offset_minutes(at.max(0.0) as u64));
    app.remote_shell = options.ui.remote_shell;
    if let Some(name) = &options.ui.shell_name {
        app.shell_name = name.clone();
    }
    let detached = kernel.detached_asks();
    let machine_changes = machines::changes(&options.services);
    // 这个进程所在的 tmux 与 ⇥ 记着的分组（tui-cockpit.md §2.1、§3.3）。
    app.tmux_socket = options
        .services
        .cockpit
        .as_ref()
        .and_then(|c| c.tmux_socket());
    app.cockpit_by_folder = cockpit::remembered(options.services.stores.as_deref()).await;
    let mut state = Loop {
        kernel,
        detached,
        options,
        app,
        cache: Cache::default(),
        renderer: Renderer::new(String::new(), String::new()).with_depth(depth(colors.level)),
        screen,
        outbox: outbox::Outbox::default(),
        events: idle(),
        pending: FuturesUnordered::new(),
        dirty: true,
        painted: Instant::now(),
        copy_after_draw: None,
        next_start: StartSource::Startup,
        services: services::ServiceState::default(),
        suspended: false,
        theme: latest::Latest::default(),
        panels: panels::Panels::default(),
        projects: projects::Projects::default(),
        stopping: false,
        user_cancel: false,
        machine_changes,
        cockpit: Default::default(),
        first_frame: None,
    };
    state.first_frame = Some(state.options.started);
    let (startup, fork) = (state.options.startup.clone(), state.options.fork);
    state.await_default_model();
    state.startup(startup, fork);
    state.fetch_commands();
    state.start_machines();
    let stop = state.options.stop.clone();
    while !state.app.quit {
        // 缺陷修复（F14）：终端关掉时（关窗口、SSH 断开）写终端先失败（EIO），SIGHUP 随后才到；以前 `draw()?`
        // 直接返回错误，回合不中止、SessionEnd 不跑、退出码不是 129。画不出来就是终端没了：按收到退出信号收尾
        // （不再画帧、中止在跑的回合、离开时 SessionEnd 的原因是 `other`，session-lifecycle.md §3.5）。
        // 这一轮界面层完成的本地命令记录（修复线 L-A）：画帧之前交给内核或新会话的草稿。
        state.flush_records();
        // 驾驶舱的监听、窗格与现场（tui-cockpit.md §1）。
        state.cockpit_sync();
        match state.draw() {
            Ok(()) => state.log_first_frame(),
            Err(_) if !state.stopping => state.stop(),
            Err(_) => {}
        }
        // transcript 的动效（回答打完、抬笔、调用落定、通知淡入）走完之前也按动画节拍重画（Puffin 的计时器）。
        // 缺陷修复（套件 peers 的偶发失败，2026-10-31）：以前画帧与这里各读一次 `Instant::now()`；淡入的最后一个 tick
        // 落在两次之间时，画出的是没点亮的一帧，这里却算它已经落定，不再重画，画面停在没点亮的样子。按画出的那一帧的
        // 时刻算，没落定就再画一帧。
        let animating = crate::ui::draw::animating(&state.app, &state.cache, state.painted);
        let notify_at = state.app.notify.next_due();
        let afk_at = state.app.afk.next_due(tokio::time::Instant::now());
        let burst_at = state.app.burst_deadline();
        tokio::select! {
            event = input.next() => match event {
                Some(event) => {
                    state.on_input(event)?;
                    if state.ready_input(&mut input).await? {
                        break;
                    }
                }
                None => break,
            },
            event = state.events.next() => {
                state.on_event(event);
                state.ready_events();
            }
            event = state.detached.next() => state.on_detached(event),
            done = state.pending.next(), if !state.pending.is_empty() => {
                if let Some(done) = done {
                    state.on_done(done);
                }
            }
            // Windows 的 paste burst：扣住的字符与缓存的冲刷时刻（tui-input.md §9.2）。
            () = tokio::time::sleep_until(tokio::time::Instant::from_std(
                burst_at.unwrap_or_else(Instant::now),
            )), if burst_at.is_some() => state.on_burst_due()?,
            () = tokio::time::sleep(crate::ui::bottom::ANIMATION), if animating => {
                state.dirty = true;
            }
            // 终端通知的计时（只是界面提示，tui-manage.md §5.1）。
            () = tokio::time::sleep_until(notify_at.unwrap_or_else(tokio::time::Instant::now)),
                if notify_at.is_some() => state.notify(),
            // 提问框的空闲计时（tools-more.md §1.1，工作线 F72）：每秒检查一次。
            () = tokio::time::sleep_until(afk_at.unwrap_or_else(tokio::time::Instant::now)),
                if afk_at.is_some() => state.afk()?,
            // 子代理有变化：重取子代理列表与后台任务的同一份快照（修复线 F34，tui-panels.md §0）。
            () = state.panels.changed() => state.refresh_tasks(),
            // 登记处或笔记变了（tui-cockpit.md §1）：按同一个查询重读。
            change = state.cockpit.changes.next() => state.cockpit_changed(change),
            // 机器的状态、延迟或清单变了（remote-exec.md §21.2）：重取快照。
            change = state.machine_changes.next() => match change {
                Some(()) => state.refresh_machines(),
                None => state.machines_ended(),
            },
            // 缺陷修复（R5-2）：原来入口不处理退出信号，SIGTERM / SIGHUP 直接结束进程，终端停在备用屏、
            // 子进程留成孤儿。现在入口把信号变成这个令牌，界面走已有的退出路径（session-lifecycle.md §3.5）。
            () = stop.cancelled(), if !state.stopping => state.stop(),
            // 子代理列表的用时与消失：没有动画时每秒重画一次（只是显示，状态仍来自快照）。
            () = tokio::time::sleep(std::time::Duration::from_secs(1)),
                if !animating
                    && (state.app.agents_ticking()
                        || state.app.machines_counting()
                        // 驾驶舱的年龄用墙上时钟（§1）。
                        || state.app.cockpit_open()) => {
                state.dirty = true;
            }
        }
    }
    // 写过窗口标题就弹回终端原来的（Puffin 离开时同）。
    if let Some(bytes) = state.app.notify.restore_title() {
        state.screen.notify(bytes);
    }
    // 写过程序状态就清掉这个终端上的记录（tui-manage.md §5.4）。
    if let Some(bytes) = state.app.program_status.leave() {
        state.screen.notify(&bytes);
    }
    // 渲染器离开它的备用屏：还回涂的地、打开自动换行（终端的其余模式由入口的守卫恢复）。
    if let Some(bytes) = state.renderer.exit() {
        let _ = state.screen.write(&bytes);
    }
    // 退出：SessionEnd(prompt_input_exit；退出信号是 other) 交给入口，在恢复终端、打印恢复提示之后跑。
    let reason = if state.stopping {
        EndReason::Other
    } else {
        EndReason::PromptInputExit
    };
    Ok(Exit {
        session: state.app.session.clone(),
        ending: Ending {
            _kernel: state.kernel.clone(),
            session: state.outbox.detach(),
            reason,
        },
    })
}

impl Loop<'_> {
    /// 注入的时钟的当前时间（Unix 毫秒）。
    pub(super) fn now_ms(&self) -> u64 {
        self.options.clock.now_ms()
    }

    /// 接上一个会话（新建的、切过去的）：句柄、画面上的会话 ID 与事件订阅（从 `from` 之后的 seq 起）在这一处写
    /// （架构整理 U10：以前打开与切换各在两处分别写 `App.session` 与 `Loop.session`）。
    pub(super) fn attach(&mut self, handle: Arc<dyn SessionHandle>, from: u64) {
        self.app.session = Some(handle.id().as_str().to_owned());
        // 这个会话的结局还没亲眼看到过：旧会话上次的结局不报成 done / error（tui-manage.md §5.4）。
        self.app.program_status.attached();
        // 订阅方落后时的重新订阅在 `follow` 里（session-lifecycle.md §2.1，F129）。
        self.events = follow(handle.clone(), from);
        self.outbox.attach(handle);
    }

    /// 发给会话的命令：命令 ID 取自注入的 Ids（落库为 `anchor.sourceCommandId`）。
    pub(super) fn envelope(&self, command: Command) -> CommandEnvelope {
        session::envelope(self.options.ids.as_ref(), command)
    }

    fn draw(&mut self) -> Result<()> {
        if !self.dirty || self.suspended || self.stopping {
            return Ok(());
        }
        self.dirty = false;
        // 窗口标题跟着会话的标题与不在终端前的标记（Puffin `window_titled`）。
        if let Some(bytes) = self.app.notify.title_bytes(self.app.view.title()) {
            self.screen.notify(&bytes);
        }
        // 程序状态跟着同一份界面状态（tui-manage.md §5.4）：变了才写。
        if self.screen.program_status() {
            let wanted = crate::ui::program_status::wanted(&self.app);
            if let Some(bytes) = self.app.program_status.bytes(&wanted) {
                self.screen.notify(&bytes);
            }
        }
        let wall = self.now_ms();
        let (width, height) = self
            .screen
            .size()
            .map_err(|error| anyhow::anyhow!("reading the terminal size: {error}"))?;
        let now = Instant::now();
        self.painted = now;
        let frame = crate::ui::draw::draw(&mut self.app, &mut self.cache, now, wall, width, height);
        // 终端的标志：涂过地时，异常退出也把终端的默认色还回去（`ui::terminal` 的恢复）。
        crate::ui::terminal::painted(frame.backdrop.is_some());
        // 画面没变时渲染器一个字节也不写（tui-kit.md §3.2）。
        if let Some(bytes) = self.renderer.render(
            usize::from(width),
            usize::from(height),
            &frame.lines,
            &[],
            frame.backdrop.as_ref(),
        ) {
            self.screen
                .write(&bytes)
                .map_err(|error| anyhow::anyhow!("drawing the terminal: {error}"))?;
        }
        self.screen.drawn(&frame, width);
        // 这一帧记下的 `@` 预览（`ui::at_view` 的 `note_at`）。
        self.fetch_at();
        if let Some(clear) = self.copy_after_draw.take() {
            self.copy_selection(clear);
        }
        Ok(())
    }

    /// paste burst 打开时（Windows，tui-input.md §9.2）：已经到了的终端输入先处理完再画一帧。Windows 上粘贴的文字是
    /// 一串按键，判定看按键的间隔（8 ms）：每个键之后都画一帧时，画得慢就把一次粘贴拆开。返回 true 表示输入结束了。
    /// 其他平台不变（每个输入之后画一帧）。
    ///
    /// 用本任务的 waker 轮询（`poll_fn`），不用 `now_or_never`：crossterm 的 `EventStream` 在没有事件时只记下第一次
    /// 轮询的 waker，用空 waker 轮询过之后，之后的按键叫不醒事件循环。
    async fn ready_input(
        &mut self,
        input: &mut (impl Stream<Item = Event> + Unpin),
    ) -> Result<bool> {
        if self.app.keys.burst.is_none() {
            return Ok(false);
        }
        for _ in 0..READY_EVENTS {
            if self.app.quit || self.suspended {
                break;
            }
            let ready = std::future::poll_fn(|cx| match input.poll_next_unpin(cx) {
                std::task::Poll::Ready(event) => std::task::Poll::Ready(Some(event)),
                std::task::Poll::Pending => std::task::Poll::Ready(None),
            })
            .await;
            match ready {
                Some(Some(event)) => self.on_input(event)?,
                Some(None) => return Ok(true),
                None => break,
            }
        }
        Ok(false)
    }

    /// 一次唤醒先处理完已经就绪的内核事件，再画一帧（tui.md §4.1）。
    ///
    /// 性能修复（修复线 F6，审查 R5-5）：以前每个流式增量各画一帧，模型输出快于画帧时界面越落越远（73 KB 的回复
    /// 落后 17 s）。画面只取决于处理完的状态，所以最后一帧不变；连续处理最多 [`READY_EVENTS`] 条，事件不停时也照样画。
    fn ready_events(&mut self) {
        for _ in 0..READY_EVENTS {
            match self.events.next().now_or_never() {
                Some(Some(event)) => self.on_event(Some(event)),
                // 流结束（会话已经不在）：`on_event` 不再等事件，这一轮到此为止。
                Some(None) => return self.on_event(None),
                None => return,
            }
        }
    }

    /// 内核事件。订阅方落后时的重新订阅在 `follow` 里（tui.md §4.1）；流结束说明会话已经不在，之后不再等事件。
    fn on_event(&mut self, event: Option<SequencedEvent>) {
        let Some(event) = event else {
            self.events = idle();
            return;
        };
        let rewritten = matches!(
            event.event,
            rust_cli_kernel_api::session_api::KernelEvent::HistoryRewritten { .. }
        );
        let finished = matches!(
            event.event,
            rust_cli_kernel_api::session_api::KernelEvent::TurnFinished { .. }
        );
        let interrupted = matches!(
            event.event,
            rust_cli_kernel_api::session_api::KernelEvent::TurnFinished {
                outcome: rust_cli_kernel_api::session_api::TurnOutcome::Interrupted,
                ..
            }
        );
        // 回合里可能新建或改了 skill 文件：回合结束后重取 skill 命令（tui.md §6）。
        if matches!(
            event.event,
            rust_cli_kernel_api::session_api::KernelEvent::TurnFinished { .. }
        ) {
            self.fetch_commands();
        }
        let now = Instant::now();
        let tasks = matches!(
            event.event,
            rust_cli_kernel_api::session_api::KernelEvent::TasksChanged { .. }
        );
        let background = manage::touches_background(&event.event);
        // 现在的会话的状态变了：← 的看板开着时重读（tui-panels.md §2.1）。
        let activity = crate::ui::sessions_status::touches_activity(&event.event);
        // footer 的请求统计（F27）：请求完成（带消息的在 assistant 提交时，没有消息的在回合结束时）后重查用量表；
        // 流式增量不触发。
        let request_done = matches!(
            event.event,
            rust_cli_kernel_api::session_api::KernelEvent::MessageDone {
                message: rust_cli_kernel_api::message::Message::Assistant(_)
            } | rust_cli_kernel_api::session_api::KernelEvent::TurnFinished { .. }
        );
        let moved = matches!(
            event.event,
            rust_cli_kernel_api::session_api::KernelEvent::WorkspaceChanged { .. }
        );
        // 目标的检查到了：目标的卡开着时重取目标（cards.rs，Puffin 的 `wants.goal`）。
        let goal_checked = matches!(
            &event.event,
            rust_cli_kernel_api::session_api::KernelEvent::MessageDone {
                message: rust_cli_kernel_api::message::Message::Attachment(attachment)
            } if attachment.source == rust_cli_kernel_api::goal::STATUS_SOURCE
        );
        let wall = self.now_ms();
        self.cockpit_event(&event.event, wall);
        self.app.view.apply(event, wall);
        // 刚到的失败被套餐的上限停下：问它什么时候回来（usage_back.rs）。
        if let Some(action) = self.app.ask_back()
            && let Err(error) = self.act(action)
        {
            self.app.error(error.to_string());
        }
        if goal_checked {
            self.refresh_goal();
        }
        // 工作目录换了（EnterWorktree / ExitWorktree / `/cd`）：`@` 补全的索引与 skill 命令属于旧目录。
        if moved {
            self.app.composer.files = Default::default();
            self.fetch_commands();
        }
        if tasks {
            self.app.snapshots.tasks.changed(&self.app.view);
        }
        // 后台任务可能变了：与子代理的变化通知发同一个查询（修复线 F34）。
        if background {
            self.refresh_tasks();
        }
        if request_done {
            self.refresh_stats();
        }
        if activity {
            self.refresh_sessions();
        }
        self.app.last_frame = now;
        self.sync_pending(now);
        let tick = tokio::time::Instant::now();
        if finished {
            self.app.notify.turn_finished(tick);
            self.app.program_status.turn_finished();
            // 退出信号之后在等的回合结束了：离开。
            if self.stopping {
                self.app.quit = true;
            }
            // esc 撤回刚发的输入（修复线 F38）。
            self.after_cancel(interrupted);
        }
        if rewritten {
            self.refetch();
        }
        self.send_deferred();
        // 缺陷修复（F45）：等回合结束的本地命令只在 `TurnFinished` 时看。内核在回合结束之后才取出排着的输入，先发
        // `QueueChanged`（队列空了）、再发下一个回合的 `TurnStarted`；以前每个事件之后都看，在这两个事件之间视图是
        // “空闲且队列空”，`/clear` 就在排着的输入的回合刚开始时执行，关闭会话时把它中止。`TurnFinished` 时视图里的
        // 队列就是内核结束这个回合时的队列：不空时内核接着开下一个回合，等它的 `TurnFinished`（tui.md §6）。
        if finished && let Err(error) = self.run_after_turn() {
            self.app.error(error.to_string());
        }
        self.dirty = true;
    }
}

impl Loop<'_> {
    /// 待处理交互可能变了：对话框的选择状态、终端通知与空闲计时跟着换。
    fn sync_pending(&mut self, now: Instant) {
        self.app.sync_interaction(now);
        let lang = self.app.lang;
        let tick = tokio::time::Instant::now();
        // 终端不在前台时每个新到的请求与该响的通知响一次铃并发终端通知（Puffin `asked`，`ui/notify.rs`）。
        let fresh = std::mem::take(&mut self.app.asks.fresh);
        let open = crate::ui::interaction_view::open_asks(&self.app);
        let any_open = !open.is_empty();
        let arrived: Vec<_> = open
            .iter()
            .filter(|(id, _)| fresh.contains(id))
            .map(|(_, request)| (*request).clone())
            .collect();
        let arrived: Vec<_> = arrived.iter().collect();
        let session = self.app.session.clone();
        let noticed = self
            .app
            .notify
            .fresh_rows(session.as_deref(), self.app.view.rows());
        let rang = self.app.notify.asked(&arrived, noticed, any_open, lang);
        if let Some(message) = rang {
            self.ring(message);
        }
        let pending = crate::ui::app::pending_of(&self.app.view, &self.app.detached);
        self.app.afk.dialog(pending, tick);
        self.app.show_countdown();
    }

    /// 响铃，终端有渠道时再发通知（tui-manage.md §5.2）。
    fn ring(&mut self, message: &str) {
        use crate::ui::notify;
        let mut bytes = notify::RING.to_owned();
        if let Some(channel) = notify::channel(&self.options.vars) {
            // 取 0–9999 的随机数；随机性取自注入的 Ids（F7：以前直接生成 uuid）。
            let id = notify::id(self.options.ids.command().as_str());
            bytes.push_str(&notify::sequence(channel, message, &self.options.vars, id));
        }
        self.screen.notify(&bytes);
    }

    /// 旧会话询问的变化（工作线 F72）；流结束（内核没有这个入口）之后不再等它。
    fn on_detached(&mut self, event: Option<rust_cli_kernel_api::orchestration::DetachedEvent>) {
        let Some(event) = event else {
            self.detached = stream::pending().boxed();
            return;
        };
        self.app.detached_changed(event);
        self.sync_pending(Instant::now());
        self.dirty = true;
    }

    /// 到了提问框的空闲检查时刻：更新倒计时；到期时按当时已选的答案提交（tools-more.md §1.1）。
    fn afk(&mut self) -> Result<()> {
        let tick = self.app.afk.tick(tokio::time::Instant::now());
        self.app.show_countdown();
        self.dirty = true;
        let Some(crate::ui::afk::Tick::Expired(timeout_ms)) = tick else {
            return Ok(());
        };
        if let Some((interaction, answer)) = self.app.idle_answer(timeout_ms) {
            self.act(crate::ui::app::Action::Resolve {
                interaction,
                answer,
            })?;
        }
        Ok(())
    }

    /// 到了通知的检查时刻：条件满足且终端有渠道时写出转义序列（tui-manage.md §5）。
    fn notify(&mut self) {
        use crate::ui::notify;
        let quiet = !self.app.running()
            && self.app.pending().is_none()
            && self.app.overlay.is_none()
            && !self.app.snapshots.background.iter().any(|entry| {
                entry.kind == rust_cli_kernel_api::manage::BackgroundKind::Agent
                    && entry.status == rust_cli_kernel_api::env_shell::BackgroundStatus::Running
            });
        let Some(due) = self
            .app
            .notify
            .due(tokio::time::Instant::now(), quiet, self.app.lang)
        else {
            return;
        };
        // 空闲提醒（旧界面多出的）：只发终端通知，不响铃。
        if let Some(channel) = notify::channel(&self.options.vars) {
            let id = notify::id(self.options.ids.command().as_str());
            self.screen.notify(&notify::sequence(
                channel,
                due.message,
                &self.options.vars,
                id,
            ));
        }
    }
}

#[path = "terminal.rs"]
mod real;

pub use real::run;
