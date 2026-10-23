//! 事件循环（docs/specs/rust-cli/tui.md §4）：终端输入、内核事件、命令的回执与动画节拍在一个循环里按序
//! 处理。只经内核会话 API（`Kernel`、`SessionHandle`、`Command`、`KernelEvent`）驱动会话；命令的回执
//! 用 `FuturesUnordered` 在循环里轮询，不另起任务。
mod actions;
mod fork_goal;
mod input;
mod lifecycle;
mod manage;
mod model_default;
mod panels;
mod projects;
mod restore;
mod services;
mod session;
mod usage;

pub use lifecycle::Startup;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use crossterm::event::Event;
use futures_util::FutureExt;
use futures_util::stream::{self, BoxStream, FuturesUnordered, Stream, StreamExt};
use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::buffer::Buffer;
use rust_cli_kernel_api::clock::{Clock, Ids};
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::provider::ModelDirectory;
use rust_cli_kernel_api::session_api::{
    Command, CommandEnvelope, EndReason, Kernel, SequencedEvent, SessionHandle, StartSource,
};
use rust_cli_platform::Recorder;

use crate::ui::app::App;
use crate::ui::palette::Palette;
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
    /// 新会话的模型与权限模式。
    pub selection: ModelSelection,
    pub mode: PermissionMode,
    pub version: String,
    /// `TERM`、`COLORTERM`、`LANG`、`LC_ALL`、`LC_MESSAGES`、`TERM_PROGRAM` 等。
    pub vars: BTreeMap<String, String>,
    /// 模型目录（`/model`）；没有时只能用 `/model 名称` 以外的方式换模型。
    pub models: Option<Arc<dyn ModelDirectory>>,
    /// 入口载入的本工作区输入历史（从旧到新）与追加入口（tui.md「输入历史」）；TUI 不做文件 IO。
    pub history: Vec<String>,
    pub recorder: Option<Recorder>,
    /// 启动时打开的会话（`--resume` / `-c`）；`fork` 为真时打开后先 fork（`--fork-session`）。
    pub startup: Startup,
    pub fork: bool,
    /// 入口注入的服务与配置值：界面语言（tui.md §7.1）。
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

/// 终端的副作用（复制）与每帧之后的回调；真实终端写 OSC 52，测试记录画面。
pub trait Screen {
    /// 用 OSC 52 把文字放进剪贴板（序列按终端复用器，`ui::clipboard::osc52`）。系统剪贴板命令不在这里：经入口的
    /// `Clipboard::copy_text`（tui-select.md §3.2）。
    fn copy(&mut self, text: &str);
    /// 每画完一帧调用一次。
    fn drawn(&mut self, _buffer: &Buffer) {}
    /// 终端通知的转义序列（tui-manage.md §5.2）：真实终端原样写到 stdout。
    fn notify(&mut self, _sequence: &str) {}
    /// 把终端交给子进程（外部编辑器，tui-input.md §4.1）：暂停读输入、离开 alternate screen 与 raw mode。
    /// 交出期间不画帧；测试里不做事。
    fn suspend(&mut self) {}
    /// 收回终端；之后整屏重画。
    fn resume(&mut self) {}
}

/// 还没有会话、或正在重新取快照时的事件流：永远不产生事件。
fn idle() -> BoxStream<'static, SequencedEvent> {
    stream::pending().boxed()
}

/// 一次唤醒最多连续处理的就绪内核事件数（之后先画一帧）。
const READY_EVENTS: usize = 256;

struct Loop<'a, B: Backend> {
    kernel: Arc<dyn Kernel>,
    options: Options,
    app: App,
    cache: Cache,
    terminal: &'a mut Terminal<B>,
    screen: &'a mut dyn Screen,
    session: Option<Arc<dyn SessionHandle>>,
    events: BoxStream<'static, SequencedEvent>,
    pending: FuturesUnordered<futures_util::future::BoxFuture<'static, Done>>,
    dirty: bool,
    /// 画完下一帧后复制选区（`true` 时复制后清掉选区）。
    copy_after_draw: Option<bool>,
    /// 运行中确认的 rewind：回合结束后发出（命令与回执提示）。
    deferred: Option<(Command, String)>,
    /// 下一个新建会话的 SessionStart 来源（`/clear` 之后是 clear）。
    next_start: StartSource,
    /// 服务操作的在途状态（界面语言的写回）。
    services: services::ServiceState,
    /// 终端交给了外部编辑器（tui-input.md §4.1）：不画帧，回来后整屏重画。
    suspended: bool,
    /// `ui.theme` 的写入在途；在途期间又选的只留最后一个（tui-manage.md §2.1）。
    theme_writing: bool,
    theme_next: Option<crate::services::UiTheme>,
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
}

/// 跑事件循环直到退出；返回退出时的会话与还没跑的 SessionEnd（[`Exit`]）。真实终端与测试（`TestBackend`）走同一个循环。
pub async fn drive<B: Backend>(
    kernel: Arc<dyn Kernel>,
    options: Options,
    palette: Palette,
    terminal: &mut Terminal<B>,
    screen: &mut dyn Screen,
    mut input: impl Stream<Item = Event> + Unpin,
) -> Result<Exit> {
    let preference = options
        .services
        .locale
        .map(crate::services::UiLocale::as_str);
    let lang = crate::ui::i18n::resolve(preference, &options.vars);
    // 配置的 `ui.theme`：dark / light 用固定的底色，auto 用探测到的背景（tui-manage.md §2.1）。
    let theme = options
        .services
        .theme
        .unwrap_or(crate::services::UiTheme::Auto);
    let detected = palette.background;
    let palette = crate::ui::palette::Palette::new(
        palette.level,
        crate::ui::slash_theme::background(theme, detected),
    )
    .with_foreground(palette.foreground);
    let mut app = App::new(
        palette,
        lang,
        Transcript::new(
            options.selection.clone(),
            options.mode,
            options.workspace.clone(),
        ),
        options.home.clone(),
        options.version.clone(),
    );
    // 滚轮每格的行数按终端（tui-select.md §4.2）；原来按终端决定要不要松开即复制，F25 起所有终端都复制。
    app.wheel = crate::ui::wheel::Accel::new(&options.vars, cfg!(windows));
    app.set_os(options.os);
    app.theme = theme;
    app.terminal_background = detected;
    app.composer.load_history(options.history.clone());
    app.memory_files = options.services.memory_files.clone();
    // npm 上有更新的版本（docs/specs/rust-cli/update.md §7，F48）：会话头下面插一个提示；启动时要打开已有会话时，
    // 第一次切换清掉旧视图后再插到新视图的顶上。
    if let Some(info) = options.services.update.clone() {
        app.push_local(crate::ui::app::Local::Update(Box::new(info.clone())));
        if !matches!(options.startup, Startup::New) {
            app.update_carry = Some(info);
        }
    }
    let mut state = Loop {
        kernel,
        options,
        app,
        cache: Cache::default(),
        terminal,
        screen,
        session: None,
        events: idle(),
        pending: FuturesUnordered::new(),
        dirty: true,
        copy_after_draw: None,
        deferred: None,
        next_start: StartSource::Startup,
        services: services::ServiceState::default(),
        suspended: false,
        theme_writing: false,
        theme_next: None,
        panels: panels::Panels::default(),
        projects: projects::Projects::default(),
        stopping: false,
        user_cancel: false,
    };
    let (startup, fork) = (state.options.startup.clone(), state.options.fork);
    state.startup(startup, fork);
    state.fetch_commands();
    let stop = state.options.stop.clone();
    while !state.app.quit {
        // 缺陷修复（F14）：终端关掉时（关窗口、SSH 断开）写终端先失败（EIO），SIGHUP 随后才到；以前 `draw()?`
        // 直接返回错误，回合不中止、SessionEnd 不跑、退出码不是 129。画不出来就是终端没了：按收到退出信号收尾
        // （不再画帧、中止在跑的回合、离开时 SessionEnd 的原因是 `other`，session-lifecycle.md §3.5）。
        if state.draw().is_err() && !state.stopping {
            state.stop();
        }
        let animating = state.app.animating();
        let notify_at = state.app.notify.next_due();
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
            // 子代理有变化：重取子代理列表与后台任务的同一份快照（修复线 F34，tui-panels.md §0）。
            () = state.panels.changed() => state.refresh_tasks(),
            // 缺陷修复（R5-2）：原来入口不处理退出信号，SIGTERM / SIGHUP 直接结束进程，终端停在备用屏、
            // 子进程留成孤儿。现在入口把信号变成这个令牌，界面走已有的退出路径（session-lifecycle.md §3.5）。
            () = stop.cancelled(), if !state.stopping => state.stop(),
            // 子代理列表的用时与消失：没有动画时每秒重画一次（只是显示，状态仍来自快照）。
            () = tokio::time::sleep(std::time::Duration::from_secs(1)),
                if !animating && state.app.agents_ticking(Instant::now()) => {
                state.dirty = true;
            }
        }
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
            session: state.session.take(),
            reason,
        },
    })
}

impl<B: Backend> Loop<'_, B> {
    /// 注入的时钟的当前时间（Unix 毫秒）。
    pub(super) fn now_ms(&self) -> u64 {
        self.options.clock.now_ms()
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
        let wall = self.now_ms();
        let (app, cache) = (&mut self.app, &mut self.cache);
        let now = Instant::now();
        let frame = self
            .terminal
            .draw(|frame| crate::ui::draw::draw(frame, app, cache, now, wall))
            .map_err(|error| anyhow::anyhow!("drawing the terminal: {error}"))?;
        self.screen.drawn(frame.buffer);
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
        if self.app.burst.is_none() {
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
                // 流结束：`on_event` 重新订阅，这一轮到此为止。
                Some(None) => return self.on_event(None),
                None => return,
            }
        }
    }

    /// 内核事件；流结束（订阅方落后）时按最后的 seq 重新订阅（tui.md §4.1）。
    fn on_event(&mut self, event: Option<SequencedEvent>) {
        let Some(event) = event else {
            self.events = match &self.session {
                Some(session) => session.subscribe(Some(self.app.view.last_seq())),
                None => idle(),
            };
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
        let wall = self.now_ms();
        self.app.view.apply(event, wall);
        // 工作目录换了（EnterWorktree / ExitWorktree / `/cd`）：`@` 补全的索引与 skill 命令属于旧目录。
        if moved {
            self.app.composer.files = Default::default();
            self.fetch_commands();
        }
        if tasks {
            self.app.tasks.changed(self.app.view.tasks(), now);
        }
        // 后台任务可能变了：与子代理的变化通知发同一个查询（修复线 F34）。
        if background {
            self.refresh_tasks();
        }
        if request_done {
            self.refresh_stats();
        }
        self.app.last_frame = now;
        self.app.sync_interaction(now);
        let lang = self.app.lang;
        let tick = tokio::time::Instant::now();
        let pending = self.app.view.pending().map(|(id, request)| (id, request));
        self.app.notify.dialog(pending, lang, tick);
        if finished {
            self.app.notify.turn_finished(tick);
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

impl<B: Backend> Loop<'_, B> {
    /// 到了通知的检查时刻：条件满足且终端有渠道时写出转义序列（tui-manage.md §5）。
    fn notify(&mut self) {
        use crate::ui::notify;
        let quiet = !self.app.running()
            && self.app.view.pending().is_none()
            && self.app.overlay.is_none()
            && !self.app.background.iter().any(|entry| {
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
        if let Some(channel) = notify::channel(&self.options.vars) {
            // 取 0–9999 的随机数；随机性取自注入的 Ids（F7：以前直接生成 uuid）。
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
