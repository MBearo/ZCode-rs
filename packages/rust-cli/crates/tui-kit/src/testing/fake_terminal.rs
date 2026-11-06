//! 测试用的假终端（docs/specs/rust-cli/tui-kit.md §3.7）：脚本化的输入字节、记下的写与生命周期。
//! 移植自 packages/tui/tui/test/src/zcode/tui/app/RuntimeSuite.scala 的 `FakeTerminal`（feat/tui a2bf878b34）。
//!
//! 句柄可以克隆：一份交给运行时，测试留一份读记录、推输入。状态在 `Arc<Mutex<..>>` 里——这是测试替身，不是运行时的
//! 状态。输入在运行时取走流之前推的会缓冲起来，之后推的直接到达。
use std::sync::{Arc, Mutex, MutexGuard};

use rust_cli_base::sync::LockUnpoisoned;

use futures_util::stream::{self, BoxStream, StreamExt};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::render::ansi;
use crate::render::terminal::{Input, Signals, Terminal};

/// 每次写之后调用的钩子：拿到写它的终端与写出的字节，测试据此推下一步的输入（看到某一帧就退出）。
type OnWrite = Box<dyn FnMut(&FakeTerminal, &str) + Send>;

#[derive(Default)]
struct State {
    writes: String,
    frames: Vec<String>,
    raw_entered: bool,
    closed: bool,
    lifecycle: Vec<String>,
    /// 与真实终端的标志相同：只在开着时关才记，所以收尾时无条件的关在没开过鼠标时什么也不记。
    mouse_on: bool,
    size: (usize, usize),
    copies: Vec<String>,
}

struct Shared {
    state: Mutex<State>,
    on_write: Mutex<Option<OnWrite>>,
    tx: UnboundedSender<Input>,
    rx: Mutex<Option<UnboundedReceiver<Input>>>,
    kitty: bool,
}

/// 脚本化的终端。`kitty` 时备用屏的设置与收尾是 kitty 键盘增强的 push / pop（与能力够的真实终端一样），否则为空。
#[derive(Clone)]
pub struct FakeTerminal(Arc<Shared>);

impl Default for FakeTerminal {
    fn default() -> FakeTerminal {
        FakeTerminal::new()
    }
}

impl FakeTerminal {
    /// 40×10、不是 kitty。
    pub fn new() -> FakeTerminal {
        FakeTerminal::build(false)
    }

    pub fn kitty() -> FakeTerminal {
        FakeTerminal::build(true)
    }

    fn build(kitty: bool) -> FakeTerminal {
        let (tx, rx) = unbounded_channel();
        FakeTerminal(Arc::new(Shared {
            state: Mutex::new(State {
                size: (40, 10),
                ..State::default()
            }),
            on_write: Mutex::new(None),
            tx,
            rx: Mutex::new(Some(rx)),
            kitty,
        }))
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.0.state.lock_unpoisoned()
    }

    /// 推一段输入字节（运行时取走流之前推的缓冲着）。一次调用是一个 [`Input::Bytes`]。
    pub fn push(&self, bytes: &[u8]) {
        // 运行时结束后接收端没了：丢掉。
        let _ = self.0.tx.send(Input::Bytes(bytes.to_vec()));
    }

    pub fn push_str(&self, s: &str) {
        self.push(s.as_bytes());
    }

    /// 改报告的尺寸，不发信号：只有运行时的轮询看得到（Scala 的 `currentSize = …`）。
    pub fn set_size(&self, width: usize, rows: usize) {
        self.state().size = (width, rows);
    }

    /// 推送式的 resize 信号（Scala 的 `resizeSink`）。
    pub fn signal_resize(&self) {
        let _ = self.0.tx.send(Input::Resize);
    }

    /// 每次写之后调用 `hook`（替换之前的）。
    pub fn on_write(&self, hook: impl FnMut(&FakeTerminal, &str) + Send + 'static) {
        *self.0.on_write.lock_unpoisoned() = Some(Box::new(hook));
    }

    /// 在生命周期记录里加一条（测试自己的步骤，如交出终端时运行的「程序」）。
    pub fn record(&self, step: &str) {
        self.state().lifecycle.push(step.to_string());
    }

    /// 写出的全部字节。
    pub fn writes(&self) -> String {
        self.state().writes.clone()
    }

    /// 每次写各自一项，按顺序。
    pub fn frames(&self) -> Vec<String> {
        self.state().frames.clone()
    }

    /// 画过的帧数：打开同步输出的写（收尾时单独的离开备用屏不算）。
    pub fn paints(&self) -> usize {
        self.state()
            .frames
            .iter()
            .filter(|f| f.contains(ansi::SYNC_BEGIN))
            .count()
    }

    pub fn raw_entered(&self) -> bool {
        self.state().raw_entered
    }

    pub fn closed(&self) -> bool {
        self.state().closed
    }

    /// 生命周期的步骤（enter_raw / enable_mouse / disable_mouse / suspend / resume / close 与 [`FakeTerminal::record`]），
    /// 按顺序。
    pub fn lifecycle(&self) -> Vec<String> {
        self.state().lifecycle.clone()
    }

    /// 经 [`Terminal::copy_to_clipboard`] 或信号句柄复制的文字。
    pub fn copies(&self) -> Vec<String> {
        self.state().copies.clone()
    }

    fn record_write(&self, s: &str) {
        {
            let mut state = self.state();
            state.writes.push_str(s);
            state.frames.push(s.to_string());
        }
        // 钩子在状态锁之外调用：它会回头读记录、推输入。钩子自己又写终端时（重入）不再调用它，不死锁。
        if let Ok(mut hook) = self.0.on_write.try_lock()
            && let Some(h) = hook.as_mut()
        {
            h(self, s);
        }
    }
}

impl Signals for FakeTerminal {
    fn write(&self, s: &str) {
        self.record_write(s);
    }

    fn copy_to_clipboard(&self, text: &str) {
        self.state().copies.push(text.to_string());
    }
}

impl Terminal for FakeTerminal {
    fn write(&mut self, s: &str) {
        self.record_write(s);
    }

    fn size(&self) -> (usize, usize) {
        self.state().size
    }

    fn input(&mut self) -> BoxStream<'static, Input> {
        match self.0.rx.lock_unpoisoned().take() {
            Some(mut rx) => stream::poll_fn(move |cx| rx.poll_recv(cx)).boxed(),
            None => stream::empty().boxed(),
        }
    }

    fn enter_raw(&mut self) -> std::io::Result<()> {
        let mut state = self.state();
        state.raw_entered = true;
        state.lifecycle.push("enter_raw".into());
        Ok(())
    }

    fn leave_raw(&mut self) {}

    fn hide_cursor(&mut self) {}

    fn show_cursor(&mut self) {}

    fn enable_mouse(&mut self) {
        let mut state = self.state();
        if !state.mouse_on {
            state.mouse_on = true;
            state.lifecycle.push("enable_mouse".into());
        }
    }

    fn disable_mouse(&mut self) {
        let mut state = self.state();
        if state.mouse_on {
            state.mouse_on = false;
            state.lifecycle.push("disable_mouse".into());
        }
    }

    fn copy_to_clipboard(&mut self, text: &str) {
        Signals::copy_to_clipboard(self, text);
    }

    fn signals(&self) -> Arc<dyn Signals> {
        Arc::new(self.clone())
    }

    fn alt_screen_setup(&self) -> String {
        if self.0.kitty {
            ansi::PUSH_KEYBOARD_ENHANCEMENT.to_string()
        } else {
            String::new()
        }
    }

    fn alt_screen_teardown(&self) -> String {
        if self.0.kitty {
            ansi::POP_KEYBOARD_ENHANCEMENT.to_string()
        } else {
            String::new()
        }
    }

    fn suspend(&mut self) {
        self.record("suspend");
    }

    fn resume(&mut self) {
        self.record("resume");
    }

    fn close(&mut self) {
        let mut state = self.state();
        state.closed = true;
        state.lifecycle.push("close".into());
    }
}
