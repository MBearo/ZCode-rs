//! Elm 架构的运行时（docs/puffin-tui.md §4.1、§6 规则 3）。
//! 移植自 packages/tui/tui/src/zcode/tui/app/Runtime.scala（feat/tui a2bf878b34）。
//!
//! 状态只在一个循环任务里（[`run`] 的 future），没有锁。事件进一个无界通道：`Cmd::Task` 与 `Cmd::Handoff` 的结果、
//! 定时器、帧节拍、尺寸轮询、握手超时、ESC 计时都由 scope 里的任务送进来；终端的输入流与订阅的源由循环自己轮询。
//! 一次收集取走此刻就绪的全部事件，作为一批折叠（Scala 的 `take` + `tryTakeN`），然后画或排一帧。
//!
//! ```text
//!  scope 的任务 ──► mpsc<Event> ─┐
//!  终端的输入流（字节、resize）──┼─► 收集一批 ─► process ─► update / reconcile ─► dirty
//!  订阅的源（按 key）─────────────┘                       └─► render_now ? 立即画 : 排一帧（frame_ms）
//! ```
//!
//! 规则（与 Scala 相同）：
//!
//! - 渲染带 dirty 标记、按帧率上限合并：一批之内的消息合成一次画；空闲的屏幕不画。按键、推送的 resize、握手落定后的那一批
//!   立即画，其余等下一个帧节拍。
//! - 首帧不等握手（§6 规则 3）。握手落定的那一批不画：下一批（收集到落定时同步送出的报告之后）才画，所以重画已经用上
//!   探测的结果。字形宽度的代数变了才整屏重画。
//! - ESC 计时按输入的序号：计时到时其间有新字节到达就什么也不做。
//! - `Cmd::Handoff` 像退出一样离开屏幕（关鼠标、离开备用屏、交出终端），程序结束后收回、整屏重画；其间不画。
//! - 后台任务都在 `spawner` 的子 scope 里（README §3.1 规则 4）：退出时先关 scope（中止任务），再按 Scala 的顺序还原
//!   终端：关鼠标 → 离开备用屏 → 退出文字 → 显示光标 → close，每步单独保护。
//!
//! 与 Scala 的不同：
//!
//! - 按 tick 的定时器送 id 而不是消息：已经移除的定时器排在队列里的 tick 不再折叠（Scala 只拦住移除之后才发的）。
//! - 一批里事件的顺序：通道的事件、终端的输入、各源的投递（Scala 是一个队列里的到达顺序）。不同来源之间的先后本来就
//!   没有定义；同一来源之内保持顺序。
//! - 交出终端期间不排帧节拍（Scala 照常每帧醒一次，只是不画）；收回时立即画。
//! - 交出终端时再来一个 `Cmd::Handoff`：丢掉它（Scala 送回一个 "already handed over" 的失败；Rust 的 `Cmd::Handoff`
//!   只有产出消息的 future，没有失败的出口）。
//! - `update` / `view` panic 时：先还原终端，再以 [`rust_cli_base::Panicked`] 的错误返回（Scala 的异常同样先经 teardown）。
//! - 返回最终的状态（入口与测试读它）。
mod subs;

use std::future::poll_fn;
use std::panic::AssertUnwindSafe;
use std::task::{Context, Poll};
use std::time::Duration;

use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use rust_cli_base::{Panicked, Spawner, catch_panic};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use super::cmd::Cmd;
use super::handshake::{self, Handshake};
use super::key::Key;
use super::key_parser::{self, KeyParser};
use super::layout::Layout;
use super::screen::{App, Viewport};
use crate::render::color::ColorDepth;
use crate::render::renderer::Renderer;
use crate::render::terminal::{Input, Terminal};
use crate::render::width;
use subs::Subs;

/// 退出时等后台任务结束的上限。任务都是可取消的（scope 的取消信号在它们的等待点上丢掉 future），通常立即结束；
/// 只有正在同步执行的 `Cmd::Fire` 会拖到这个上限，过了就中止。
const CLOSE_GRACE: Duration = Duration::from_secs(1);

pub struct RuntimeConfig {
    /// 帧节拍：dirty 之后最多等这么久画。
    pub frame_ms: u64,
    pub quit_key: Key,
    /// 尺寸轮询的间隔：给不发 resize 信号的终端（tmux、某些 PTY）兜底。
    pub width_poll_ms: u64,
    pub enable_mouse: bool,
    /// 启动时与终端的协商（宽度与颜色的探测）。只给真实终端：无头终端从不回答，只会走超时。
    pub handshake: Option<Box<dyn Handshake>>,
    pub handshake_timeout_ms: u64,
    /// 单独的 ESC 等下一个字节多久才算 Esc（docs/puffin-tui.md §4.3）。只有传统编码需要它。
    pub escape_ms: u64,
    /// 终端的颜色深度：渲染器按它降采样（docs/specs/rust-cli/tui-kit.md §3.4）。缺省真彩。
    pub depth: ColorDepth,
}

impl Default for RuntimeConfig {
    fn default() -> RuntimeConfig {
        RuntimeConfig {
            frame_ms: 16,
            quit_key: Key::Ctrl('Q'),
            width_poll_ms: 500,
            enable_mouse: false,
            handshake: None,
            handshake_timeout_ms: handshake::TIMEOUT.as_millis() as u64,
            escape_ms: key_parser::ESCAPE_TIMEOUT.as_millis() as u64,
            depth: ColorDepth::TrueColor,
        }
    }
}

impl std::fmt::Debug for RuntimeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeConfig")
            .field("frame_ms", &self.frame_ms)
            .field("quit_key", &self.quit_key)
            .field("width_poll_ms", &self.width_poll_ms)
            .field("enable_mouse", &self.enable_mouse)
            .field("handshake", &self.handshake.is_some())
            .field("handshake_timeout_ms", &self.handshake_timeout_ms)
            .field("escape_ms", &self.escape_ms)
            .field("depth", &self.depth)
            .finish()
    }
}

/// 循环消费的东西。
enum Event<M> {
    /// 一段输入字节（终端的一次读）。
    Input(Vec<u8>),
    Message(M),
    /// 订阅的源 `source` 送来的值；源已经退订时丢掉。
    Delivery {
        source: u64,
        msg: M,
    },
    /// 定时器 `id` 到了一次。
    Tick(u64),
    Frame,
    /// 重读终端尺寸；`push` 是来自 OS 的 resize 信号（立即画），不是兜底的轮询（下一帧画）。
    SizeCheck {
        push: bool,
    },
    HandshakeTimeout,
    /// 输入的第 `seq` 段留下一个等着的 ESC 时起的计时。
    EscapeTimeout(u64),
    /// 交出终端的程序结束了：收回终端，再折叠消息。
    Resumed(M),
}

/// 跑应用直到退出（退出键或 `Cmd::Quit`），返回最终的状态。返回之前后台任务都已停下、终端已还原。
///
/// 任务经 `spawner` 的子 scope 启动；`terminal` 的输入流只取一次。进不了原始模式时还原终端并返回错误；应用 panic 时
/// 还原终端并返回 [`Panicked`] 的错误。
pub async fn run<A>(
    app: A,
    terminal: Box<dyn Terminal>,
    config: RuntimeConfig,
    spawner: Spawner,
) -> anyhow::Result<A::State>
where
    A: App,
    A::Msg: Clone + PartialEq,
{
    // init 在碰终端之前（Scala 在 Loop 的构造里）：它 panic 时没有要还原的东西。捕获点清掉 panic hook 在本线程记下的
    // 文字（照 tui.md §7.9「panic 与终端」），同 base 的捕获点。
    let (state, init_cmd) =
        std::panic::catch_unwind(AssertUnwindSafe(|| app.init())).map_err(|payload| {
            rust_cli_base::take_panic_note();
            Panicked {
                message: panic_message(payload.as_ref()),
            }
        })?;
    let scope = spawner.child("puffin-runtime");
    let mut lp = Loop::new(app, state, terminal, config, scope.spawner());
    if let Err(error) = lp.setup() {
        lp.teardown();
        return Err(error.into());
    }
    let outcome = catch_panic(lp.drive(init_cmd)).await;
    // 先停后台任务（Scala 先释放 Supervisor），再还原终端。
    scope.close(CLOSE_GRACE).await;
    lp.teardown();
    match outcome {
        Ok(()) => Ok(lp.state),
        Err(panicked) => Err(panicked.into()),
    }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_string())
}

/// 跑一个步骤，吞掉它的 panic：收尾的一步失败不能跳过之后的步骤（尤其是离开原始模式的 close）。
///
/// 吞掉时清掉 panic hook 在本线程记下的文字（照 tui.md §7.9「panic 与终端」）：收尾时终端还在原始模式里，hook
/// 记下了它；不清的话，这个线程上的下一次 panic 会被当成展开中又 panic，hook 就把终端还原了。
fn guarded(step: impl FnOnce()) {
    if std::panic::catch_unwind(AssertUnwindSafe(step)).is_err() {
        rust_cli_base::take_panic_note();
    }
}

struct Loop<A: App> {
    app: A,
    state: A::State,
    terminal: Box<dyn Terminal>,
    config: RuntimeConfig,
    renderer: Renderer,
    spawner: Spawner,
    tx: UnboundedSender<Event<A::Msg>>,
    rx: UnboundedReceiver<Event<A::Msg>>,
    input: Option<BoxStream<'static, Input>>,
    subs: Subs<A::Msg>,
    parser: KeyParser,
    /// 还在进行的启动握手；`complete_handshake` 清掉它（只一次）。它不扣住首帧（§6 规则 3）。
    handshake: Option<Box<dyn Handshake>>,
    quit: bool,
    resize_pending: bool,
    width: usize,
    rows: usize,
    dirty: bool,
    /// 在这一批的末尾就画，不等帧节拍（按键、推送的 resize、握手落定）。
    render_now: bool,
    /// 握手在这一批里落定了：这一批不画，下一批画。
    settling: bool,
    /// 上一批落定了握手：这一批不阻塞地收集（落定时同步送出的报告），处理完就画（Scala 的 `Event.Settled`）。
    settle_pending: bool,
    /// 一个帧节拍在路上。只在有东西 dirty 时才排，空闲的屏幕不醒。
    frame_armed: bool,
    /// 终端在别的程序手里（`Cmd::Handoff`）：什么也不画。
    handed_off: bool,
    /// 到目前为止的输入段数，与上次起 ESC 计时时的段数：计时到时只在其间没有新字节时冲掉 ESC。
    input_seq: u64,
    escape_armed_at: Option<u64>,
}

impl<A> Loop<A>
where
    A: App,
    A::Msg: Clone + PartialEq,
{
    fn new(
        app: A,
        state: A::State,
        terminal: Box<dyn Terminal>,
        mut config: RuntimeConfig,
        spawner: Spawner,
    ) -> Loop<A> {
        let renderer = Renderer::new(terminal.alt_screen_setup(), terminal.alt_screen_teardown())
            .with_depth(config.depth);
        let (width, rows) = terminal.size();
        let (tx, rx) = unbounded_channel();
        let handshake = config.handshake.take();
        Loop {
            app,
            state,
            terminal,
            config,
            renderer,
            spawner,
            tx,
            rx,
            input: None,
            subs: Subs::new(),
            parser: KeyParser::new(),
            handshake,
            quit: false,
            resize_pending: false,
            width,
            rows,
            dirty: true,
            render_now: false,
            settling: false,
            settle_pending: false,
            frame_armed: false,
            handed_off: false,
            input_seq: 0,
            escape_armed_at: None,
        }
    }

    // ---- 启动 ----

    fn setup(&mut self) -> std::io::Result<()> {
        self.terminal.enter_raw()?;
        self.terminal.hide_cursor();
        if self.config.enable_mouse {
            self.terminal.enable_mouse();
        }
        Ok(())
    }

    async fn drive(&mut self, init_cmd: Cmd<A::Msg>) {
        self.start(init_cmd);
        while !self.quit {
            let batch = self.gather().await;
            self.process(batch);
        }
    }

    fn start(&mut self, init_cmd: Cmd<A::Msg>) {
        self.input = Some(self.terminal.input());
        // 请求在接好输入之后、首帧之前写出：回答不会早于读它的流，探测自己的擦行落在首帧随后画的那一行上。
        if let Some(h) = &self.handshake {
            let request = h.request();
            self.terminal.write(&request);
        }
        self.exec(init_cmd);
        self.reconcile();
        // 首帧：从不推迟（见 `handshake`）。
        self.render_if_needed();
        self.arm_frame();
        let tx = self.tx.clone();
        let poll = Duration::from_millis(self.config.width_poll_ms.max(1));
        self.spawner
            .spawn_resident("puffin-runtime: size poll", async move {
                loop {
                    tokio::time::sleep(poll).await;
                    if tx.send(Event::SizeCheck { push: false }).is_err() {
                        break;
                    }
                }
            });
        if self.handshake.is_some() {
            self.send_after(self.config.handshake_timeout_ms, Event::HandshakeTimeout);
        }
    }

    // ---- 收集一批 ----

    /// 等到至少一个事件就绪，取走此刻就绪的全部。上一批落定了握手时不等：空批也立即返回。
    async fn gather(&mut self) -> Vec<Event<A::Msg>> {
        let settle = self.settle_pending;
        poll_fn(|cx| {
            let mut out = Vec::new();
            self.collect(cx, &mut out);
            if out.is_empty() && !settle {
                Poll::Pending
            } else {
                Poll::Ready(out)
            }
        })
        .await
    }

    fn collect(&mut self, cx: &mut Context<'_>, out: &mut Vec<Event<A::Msg>>) {
        while let Poll::Ready(Some(event)) = self.rx.poll_recv(cx) {
            out.push(event);
        }
        if let Some(input) = self.input.as_mut() {
            loop {
                match input.poll_next_unpin(cx) {
                    Poll::Ready(Some(Input::Bytes(bytes))) => out.push(Event::Input(bytes)),
                    Poll::Ready(Some(Input::Resize)) => out.push(Event::SizeCheck { push: true }),
                    // 输入关了（stdin 到头）：不再读。
                    Poll::Ready(None) => {
                        self.input = None;
                        break;
                    }
                    Poll::Pending => break,
                }
            }
        }
        self.subs.collect(cx, out);
    }

    // ---- 折叠一批 ----

    fn process(&mut self, batch: Vec<Event<A::Msg>>) {
        let settled = std::mem::take(&mut self.settle_pending);
        for event in batch {
            if self.quit {
                break;
            }
            match event {
                Event::Input(bytes) => {
                    // 已经缓冲的键（粘贴、快速打字）在同一批里到达：合成一次画，并且立即画。
                    self.input_seq += 1;
                    self.feed_input(&bytes);
                    self.render_now = true;
                }
                Event::Message(msg) => self.apply(msg),
                Event::Delivery { source, msg } => {
                    if self.subs.is_live(source) {
                        self.apply(msg);
                    }
                }
                Event::Tick(id) => {
                    if let Some(msg) = self.subs.tick(id) {
                        self.apply(msg);
                    }
                }
                Event::Frame => {
                    self.frame_armed = false;
                    self.render_now = true;
                }
                Event::SizeCheck { push } => {
                    if self.check_size() && push {
                        self.render_now = true;
                    }
                }
                Event::EscapeTimeout(seq) => {
                    // ESC 之后没有字节：它是 Esc。到达过的字节推进了 `input_seq`，过时的计时什么也不做。
                    if seq == self.input_seq && self.parser.pending_escape() {
                        for key in self.parser.flush_escape() {
                            if !self.quit {
                                self.handle_key(key);
                            }
                        }
                        self.render_now = true;
                    }
                }
                // 给从不回答（或答一半）的终端兜底：按已经到达的落定。
                Event::HandshakeTimeout => self.complete_handshake(),
                Event::Resumed(msg) => {
                    // 程序退出：收回终端；渲染器已离开备用屏，下一帧重新进入并整屏重画。
                    self.terminal.resume();
                    self.terminal.hide_cursor();
                    if self.config.enable_mouse {
                        self.terminal.enable_mouse();
                    }
                    self.handed_off = false;
                    self.check_size();
                    self.dirty = true;
                    self.apply(msg);
                    self.render_now = true;
                }
            }
        }
        if settled {
            self.render_now = true;
        }
        if self.settling {
            self.settling = false;
            self.render_now = false;
            self.settle_pending = true;
        }
        if self.quit {
            return;
        }
        if self.parser.pending_escape() && self.escape_armed_at != Some(self.input_seq) {
            self.escape_armed_at = Some(self.input_seq);
            self.send_after(self.config.escape_ms, Event::EscapeTimeout(self.input_seq));
        }
        if self.render_now {
            self.render_now = false;
            self.render_if_needed();
        }
        self.arm_frame();
    }

    /// 帧率上限：还 dirty 的等一个节拍，下 `frame_ms` 里的消息都并进同一次画。
    fn arm_frame(&mut self) {
        if (self.dirty || self.resize_pending)
            && !self.quit
            && !self.frame_armed
            && !self.handed_off
        {
            self.frame_armed = true;
            self.send_after(self.config.frame_ms, Event::Frame);
        }
    }

    /// `ms` 毫秒后送 `event`。
    fn send_after(&self, ms: u64, event: Event<A::Msg>) {
        let tx = self.tx.clone();
        let wait = Duration::from_millis(ms);
        self.spawner
            .spawn_resident("puffin-runtime: timer", async move {
                tokio::time::sleep(wait).await;
                let _ = tx.send(event);
            });
    }

    // ---- 输入 ----

    /// 握手进行时它挡在解析器前面：吞下终端的回答，用户的输入照常通过。
    fn feed_input(&mut self, bytes: &[u8]) {
        for (i, &b) in bytes.iter().enumerate() {
            if self.quit {
                return;
            }
            let Some(h) = self.handshake.as_mut() else {
                self.feed_parser(&bytes[i..]);
                return;
            };
            let step = h.feed(b);
            if !step.passthrough.is_empty() {
                self.feed_parser(&step.passthrough);
            }
            if step.done {
                self.complete_handshake();
            }
        }
    }

    fn feed_parser(&mut self, bytes: &[u8]) {
        for key in self.parser.feed(bytes) {
            if self.quit {
                return;
            }
            self.handle_key(key);
        }
    }

    /// 一个键折进状态（或请求退出）。
    fn handle_key(&mut self, key: Key) {
        if key == self.config.quit_key {
            self.quit = true;
        } else if let Some(msg) = self.subs.key(&key, Viewport::new(self.width, self.rows)) {
            self.apply(msg);
        }
    }

    /// 幂等（回答完成与超时都可能到这里）：落定握手、重放扣着的字节、按采纳的宽度重画。
    fn complete_handshake(&mut self) {
        let Some(mut h) = self.handshake.take() else {
            return;
        };
        let generation = width::generation();
        let settled = h.settle();
        if let Some(measured) = settled.widths {
            measured.adopt();
        }
        self.feed_parser(&settled.replay);
        // 宽度变了任何一行都可能变：整屏重画。没变时画过的格子都对，按格子差异就行（修复原因：以前无条件清屏重画，
        // 不支持同步输出的终端闪一下空屏）。
        if width::generation() != generation {
            self.exec(Cmd::Refresh);
        }
        // 修复原因：探测结果经回调同步送进订阅的源，排在本批之后；以前这里立即画，先按旧状态重画一遍。现在本批不画，
        // 下一批收集到结果之后再画。
        self.settling = true;
    }

    fn check_size(&mut self) -> bool {
        let (width, rows) = self.terminal.size();
        if (width, rows) != (self.width, self.rows) {
            self.width = width;
            self.rows = rows;
            self.resize_pending = true;
            true
        } else {
            false
        }
    }

    // ---- update 与 Cmd ----

    fn apply(&mut self, msg: A::Msg) {
        let cmd = self.app.update(msg, &mut self.state);
        self.dirty = true;
        self.exec(cmd);
        self.reconcile();
    }

    fn reconcile(&mut self) {
        let wanted = self.app.subscriptions(&self.state);
        self.subs.reconcile(wanted, &self.spawner, &self.tx);
    }

    fn exec(&mut self, cmd: Cmd<A::Msg>) {
        match cmd {
            Cmd::None => {}
            Cmd::Quit => self.quit = true,
            Cmd::Refresh => {
                // 从头重画：下一帧清屏并重画每个格子。
                self.renderer.invalidate();
                self.dirty = true;
            }
            Cmd::Batch(cmds) => {
                for c in cmds {
                    self.exec(c);
                }
            }
            Cmd::Fire(effect) => {
                self.spawner
                    .spawn_resident("puffin-runtime: fire", async move { effect() });
            }
            Cmd::Task(work) => {
                let tx = self.tx.clone();
                self.spawner
                    .spawn_resident("puffin-runtime: task", async move {
                        let msg = work.await;
                        let _ = tx.send(Event::Message(msg));
                    });
            }
            Cmd::Handoff(work) => self.handoff(work),
        }
    }

    /// 像退出一样离开屏幕（关鼠标、离开备用屏、显示光标、离开原始模式），然后跑程序；[`Event::Resumed`] 收回。
    fn handoff(&mut self, work: futures_util::future::BoxFuture<'static, A::Msg>) {
        if self.handed_off {
            return;
        }
        self.handed_off = true;
        self.terminal.disable_mouse();
        if let Some(exit) = self.renderer.exit() {
            self.terminal.write(&exit);
        }
        self.terminal.suspend();
        let tx = self.tx.clone();
        self.spawner
            .spawn_resident("puffin-runtime: handoff", async move {
                let msg = work.await;
                let _ = tx.send(Event::Resumed(msg));
            });
    }

    // ---- 画 ----

    fn render(&mut self) {
        let (width, rows) = (self.width, self.rows);
        // resize 不需要别的：尺寸与上一帧不同时渲染器自己清屏重画。
        self.resize_pending = false;
        let screen = self.app.view(&self.state, Viewport::new(width, rows));
        let lines = Layout::into_lines(screen.body, width);
        if let Some(frame) = self.renderer.render(
            width,
            rows,
            &lines,
            &screen.graphics,
            screen.backdrop.as_ref(),
        ) {
            self.terminal.write(&frame);
        }
        self.dirty = false;
    }

    /// 画还没画的变化。resize 在两种模式下都只驱动一次画（`render` 总会消费 `resize_pending`），不会每帧空转。
    fn render_if_needed(&mut self) {
        if (self.dirty || self.resize_pending) && !self.quit && !self.handed_off {
            self.render();
        }
    }

    // ---- 收尾 ----

    fn teardown(&mut self) {
        self.quit = true;
        self.input = None;
        guarded(|| self.subs.clear());
        // 顺序：关鼠标 → 离开备用屏 → 应用的退出文字 → 显示光标 → close（离开原始模式）。先关鼠标，还原期间不会再来
        // 报告。离开备用屏时也 pop 备用屏的 kitty 键盘增强（在 `?1049l` 之前）；close 再 pop 主屏那一份——每块缓冲一对。
        guarded(|| self.terminal.disable_mouse());
        guarded(|| {
            if let Some(exit) = self.renderer.exit() {
                self.terminal.write(&exit);
            }
        });
        guarded(|| {
            let text = self.app.exit_text(&self.state);
            if !text.is_empty() {
                self.terminal.write(&text);
            }
        });
        guarded(|| self.terminal.show_cursor());
        guarded(|| self.terminal.close());
    }
}
