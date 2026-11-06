//! 真实的 TTY：原始模式、输入字节、尺寸、kitty 键盘增强、鼠标、括号粘贴、
//! 焦点与外观报告、OSC 52 复制、交出终端（编辑器）。
//! 移植自 packages/tui/tui/src/zcode/tui/platform/NodeTerminal.scala（feat/tui a2bf878b34）；OSC 52 的复用器包装取自旧
//! tui 的 `ui/clipboard.rs`，带超时的 poll / read 取自旧 tui 的 `ui/palette.rs`。
//!
//! 输入：一个阻塞的读任务（经入口给的 `Spawner::spawn_blocking` 启动）把读到的字节经 tokio 的通道交给
//! [`Terminal::input`] 的流；按键解码在运行时的 `KeyParser` 里（纯函数）。
//!
//! - **unix**：rustix 的 `poll` 等 fd 0 可读，再 `read`。`poll` 按 [`SLICE`] 分片超时，读任务因此能看到 `close` 并退出
//!   （阻塞任务不能中止）。交出终端时读任务停在条件变量上；读与 `suspend` 在同一把锁里，`suspend` 返回之后不会再读走
//!   一个字节——它们属于接手的程序。
//! - **windows**：控制台的输入是记录，不是字节流。读任务用 crossterm 的 `poll` / `read` 读事件，再按 `KeyParser` 认得的
//!   方言编码回字节（[`encode`]）：带修饰的键是 kitty 的 `CSI 码;修饰 u`（Esc 也是 `CSI 27 u`，没有 ESC 的歧义与 50 ms
//!   的等待），文字是 UTF-8，方向等功能键是 `CSI 1;修饰 X` / `CSI n;修饰 ~`，鼠标是 SGR 1006，粘贴是括号粘贴，焦点是
//!   `CSI I` / `CSI O`，尺寸变化是 [`Input::Resize`]。Ctrl+Alt 加非字母数字的字是 AltGr 打出的字，按文字送。所以
//!   windows 上不写 kitty push、括号粘贴、焦点与外观报告的 VT 开关（它们会让终端把回答塞进输入记录），鼠标用 crossterm
//!   的控制台模式打开。
//!
//! 推送式的 resize：入口可以给一个 `BoxStream<'static, ()>`（它从 OS 信号造；本 crate 不能用 tokio 的 `signal`），
//! 每项成为一个 [`Input::Resize`]。运行时 500 ms 的尺寸轮询仍是兜底。
//!
//! 不读环境变量：入口把读好的变量交进来（`TERM`、`TERM_PROGRAM`、`TERM_PROGRAM_VERSION`、`KITTY_WINDOW_ID`、`TMUX`、
//! `STY`），kitty 键盘的级别按 [`keyboard_enhancement`] 选。
//!
//! 与 Scala 的不同：tmux 里 Scala 用 `tmux load-buffer -w -` 复制（tmux 缺省丢掉应用的 OSC 52）。本 crate 不起进程
//! （§2.3），复制的途径归入口的剪贴板服务；这里只写 OSC 52：tmux 里原样一份再加一份 DCS 透传，screen 里分段的 DCS。
use std::collections::BTreeMap;
use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use base64::Engine;
use futures_util::stream::{self, BoxStream, StreamExt};
use rust_cli_base::Spawner;
use rust_cli_base::names::env;
use rust_cli_base::sync::{CondvarUnpoisoned, LockUnpoisoned};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use super::terminal::{Input, Signals, Terminal};
use super::{ansi, panic_log};
use crate::app::handshake::keyboard_enhancement;

/// 读任务一次等输入的最长时间：它检查 `close` 的间隔。
const SLICE: Duration = Duration::from_millis(100);

/// 终端复用器：OSC 52 要包一层。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mux {
    None,
    Tmux,
    Screen,
}

impl Mux {
    /// 按入口给的变量判断（`TMUX`、`STY` 非空）。
    pub fn from_vars(vars: &BTreeMap<String, String>) -> Mux {
        let set = |k: &str| vars.get(k).is_some_and(|v| !v.is_empty());
        if set("TMUX") {
            Mux::Tmux
        } else if set("STY") {
            Mux::Screen
        } else {
            Mux::None
        }
    }
}

/// OSC 52 复制 `text`：tmux 里原样一份再加一份 DCS 透传；screen 里按 76 个字符分段的 DCS；`st` 时用 ST（`ESC \`）结尾
/// （kitty），否则用 BEL。
pub fn osc52(text: &str, mux: Mux, st: bool) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    match mux {
        Mux::Tmux => {
            let raw = ansi::osc52_copy(&encoded);
            let wrapped = format!("\x1bPtmux;{}\x1b\\", raw.replace('\x1b', "\x1b\x1b"));
            format!("{raw}{wrapped}")
        }
        Mux::Screen => {
            let chunks: Vec<&str> = encoded
                .as_bytes()
                .chunks(76)
                .map(|chunk| std::str::from_utf8(chunk).unwrap_or_default())
                .collect();
            format!("\x1bP\x1b]52;c;{}\x07\x1b\\", chunks.join("\x1b\\\x1bP"))
        }
        Mux::None if st => format!("{}52;c;{encoded}{}", ansi::OSC, ansi::ST),
        Mux::None => ansi::osc52_copy(&encoded),
    }
}

/// 读任务的闸：交出终端时停读，关闭时退出。
#[derive(Default)]
struct Gate {
    state: Mutex<GateState>,
    changed: Condvar,
}

#[derive(Default)]
struct GateState {
    paused: bool,
    closed: bool,
}

impl Gate {
    fn lock(&self) -> MutexGuard<'_, GateState> {
        self.state.lock_unpoisoned()
    }

    fn set(&self, f: impl FnOnce(&mut GateState)) {
        f(&mut self.lock());
        self.changed.notify_all();
    }

    /// 等到不再暂停；关闭了返回 `false`。
    fn wait_open(&self) -> bool {
        let mut state = self.lock();
        while state.paused && !state.closed {
            state = self.changed.wait_unpoisoned(state);
        }
        !state.closed
    }
}

/// 写到 stdout 的一侧：帧与信号共用。每次写在 stdout 的锁里整段写出。
struct Out {
    closed: AtomicBool,
    mux: Mux,
    st: bool,
}

impl Out {
    fn write(&self, s: &str) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        let mut out = std::io::stdout().lock();
        // 写失败（终端没了）只丢掉这一次。
        let _ = out.write_all(s.as_bytes());
        let _ = out.flush();
    }
}

impl Signals for Out {
    fn write(&self, s: &str) {
        Out::write(self, s);
    }

    fn copy_to_clipboard(&self, text: &str) {
        Out::write(self, &osc52(text, self.mux, self.st));
    }
}

/// 真实的 TTY。见模块文档。
pub struct RealTerminal {
    out: Arc<Out>,
    gate: Arc<Gate>,
    spawner: Spawner,
    resize: Option<BoxStream<'static, ()>>,
    input_taken: bool,
    /// 这个终端的 kitty 键盘 push（有的话）；它的 pop 总是 `CSI < u`。
    keyboard_push: Option<&'static str>,
    mouse_on: bool,
    closed: bool,
}

impl RealTerminal {
    /// stdin 与 stdout 都是终端。否则入口用无头终端。
    pub fn available() -> bool {
        std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
    }

    /// `vars` 是入口读好的环境变量；`resize` 是 OS 的 resize 信号（没有时只靠运行时的轮询）；读任务经 `spawner` 启动。
    pub fn new(
        vars: &BTreeMap<String, String>,
        resize: Option<BoxStream<'static, ()>>,
        spawner: Spawner,
    ) -> RealTerminal {
        let term = vars.get(env::TERM).map_or("", String::as_str);
        let kitty = term.contains("kitty") || vars.contains_key(env::KITTY_WINDOW_ID);
        // windows 的输入是控制台记录，不经 VT：kitty 的 push 没有用处。
        let keyboard_push = if cfg!(windows) {
            None
        } else {
            keyboard_enhancement(vars)
        };
        RealTerminal {
            out: Arc::new(Out {
                closed: AtomicBool::new(false),
                mux: Mux::from_vars(vars),
                st: kitty,
            }),
            gate: Arc::new(Gate::default()),
            spawner,
            resize,
            input_taken: false,
            keyboard_push,
            mouse_on: false,
            closed: false,
        }
    }

    /// 本终端打开的输入模式的开关序列（unix：括号粘贴、焦点、外观报告，与 kitty 的 push / pop）。
    fn modes(&self, on: bool) -> String {
        let mut s = String::new();
        if cfg!(windows) {
            return s;
        }
        if on {
            if let Some(push) = self.keyboard_push {
                s.push_str(push);
            }
            // 与鼠标一样，括号粘贴几乎处处支持、不支持的忽略，所以不按 `keyboard_push` 判断；它是全局的 DEC 模式（不按
            // 屏幕缓冲记），开一次主屏与备用屏都有。
            s.push_str(ansi::BRACKETED_PASTE_ENABLE);
            // 焦点报告：请求到达时终端不在前台才响铃、改标题（docs/puffin-tui.md §4.2「不在终端前时」）。
            s.push_str(ansi::FOCUS_ENABLE);
            // 外观报告（DEC 2031）：终端换深浅色时主题当场换侧（docs/puffin-tui.md §4.2「主题」）。
            s.push_str(ansi::APPEARANCE_REPORTS_ON);
        } else {
            s.push_str(ansi::BRACKETED_PASTE_DISABLE);
            s.push_str(ansi::FOCUS_DISABLE);
            s.push_str(ansi::APPEARANCE_REPORTS_OFF);
            if self.keyboard_push.is_some() {
                s.push_str(ansi::POP_KEYBOARD_ENHANCEMENT);
            }
        }
        s
    }
}

impl Terminal for RealTerminal {
    fn write(&mut self, s: &str) {
        self.out.write(s);
    }

    fn size(&self) -> (usize, usize) {
        match crossterm::terminal::size() {
            Ok((w, h)) if w > 0 && h > 0 => (usize::from(w), usize::from(h)),
            _ => (80, 24),
        }
    }

    fn input(&mut self) -> BoxStream<'static, Input> {
        if self.input_taken {
            return stream::empty().boxed();
        }
        self.input_taken = true;
        let (tx, mut rx) = unbounded_channel();
        let gate = self.gate.clone();
        self.spawner
            .spawn_blocking("tui-kit: terminal input", move || read_loop(&gate, &tx));
        let bytes = stream::poll_fn(move |cx| rx.poll_recv(cx));
        match self.resize.take() {
            Some(resize) => stream::select(bytes, resize.map(|()| Input::Resize)).boxed(),
            None => bytes.boxed(),
        }
    }

    fn enter_raw(&mut self) -> std::io::Result<()> {
        crossterm::terminal::enable_raw_mode()?;
        let modes = self.modes(true);
        self.out.write(&modes);
        panic_log::entered(self.keyboard_push.is_some());
        Ok(())
    }

    fn leave_raw(&mut self) {
        // 已经不在原始模式（或终端没了）：没有要还原的。
        let _ = crossterm::terminal::disable_raw_mode();
    }

    fn hide_cursor(&mut self) {
        self.out.write(ansi::HIDE_CURSOR);
    }

    fn show_cursor(&mut self) {
        self.out.write(ansi::SHOW_CURSOR);
    }

    // SGR 鼠标报告几乎处处支持、不支持的忽略，所以不按 `keyboard_push` 判断。关只在开着时写，收尾时不会给从没开过的
    // 终端写多余的复位。
    fn enable_mouse(&mut self) {
        if !self.mouse_on {
            mouse(&self.out, true);
            self.mouse_on = true;
        }
    }

    fn disable_mouse(&mut self) {
        if self.mouse_on {
            mouse(&self.out, false);
            self.mouse_on = false;
        }
    }

    fn copy_to_clipboard(&mut self, text: &str) {
        Signals::copy_to_clipboard(self.out.as_ref(), text);
    }

    fn signals(&self) -> Arc<dyn Signals> {
        self.out.clone()
    }

    // kitty 的键盘模式按屏幕缓冲记：`enter_raw` 在主屏的 push 到不了备用屏，渲染器进入时再 push、离开时 pop。与
    // `enter_raw` 同一个 push，不是 kitty 的终端从不见到它（会让按键重复）。
    fn alt_screen_setup(&self) -> String {
        self.keyboard_push.unwrap_or_default().to_string()
    }

    fn alt_screen_teardown(&self) -> String {
        if self.keyboard_push.is_some() {
            ansi::POP_KEYBOARD_ENHANCEMENT.to_string()
        } else {
            String::new()
        }
    }

    // 交出终端（编辑器，docs/puffin-tui.md §4.2「设置」）：先停读、关掉本终端打开的输入模式，子程序才能独占输入；鼠标由
    // 运行时在交出前关掉。
    fn suspend(&mut self) {
        self.gate.set(|g| g.paused = true);
        let mut s = self.modes(false);
        s.push_str(ansi::SHOW_CURSOR);
        self.out.write(&s);
        self.leave_raw();
    }

    fn resume(&mut self) {
        // 回不到原始模式时照常继续：输入仍然到达，只是带回显。
        let _ = self.enter_raw();
        self.gate.set(|g| g.paused = false);
    }

    fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.disable_mouse();
        let mut s = self.modes(false);
        s.push_str(ansi::SHOW_CURSOR);
        self.out.write(&s);
        self.leave_raw();
        panic_log::left();
        self.out.closed.store(true, Ordering::Release);
        self.gate.set(|g| g.closed = true);
    }
}

impl Drop for RealTerminal {
    /// 运行时之外的退出路径（入口出错、panic 的展开）也把终端还回去。
    ///
    /// 展开到这里的 panic（照 tui.md §7.9「panic 与终端」）：运行时的收尾没跑，还在备用屏里，先离开它，再还原终端，
    /// 最后把 hook 记下的文字写到 stderr（hook 在终端还在界面里时不写）。
    fn drop(&mut self) {
        let panicking = std::thread::panicking();
        if panicking && !self.closed {
            let leave = format!(
                "{}{}{}",
                ansi::WRAP_ON,
                self.alt_screen_teardown(),
                ansi::ALT_SCREEN_EXIT
            );
            self.out.write(&leave);
        }
        self.close();
        if panicking && let Some(text) = panic_log::take_note() {
            let _ = std::io::stderr().write_all(text.as_bytes());
        }
    }
}

#[cfg(unix)]
fn mouse(out: &Out, on: bool) {
    out.write(if on {
        ansi::MOUSE_ENABLE
    } else {
        ansi::MOUSE_DISABLE
    });
}

#[cfg(windows)]
fn mouse(out: &Out, on: bool) {
    // 控制台的鼠标输入是控制台模式，不是 VT 开关：crossterm 的命令在 windows 上设控制台模式。
    if out.closed.load(Ordering::Acquire) {
        return;
    }
    let mut stdout = std::io::stdout();
    let _ = if on {
        crossterm::execute!(stdout, crossterm::event::EnableMouseCapture)
    } else {
        crossterm::execute!(stdout, crossterm::event::DisableMouseCapture)
    };
}

/// unix 的读任务：见模块文档。
#[cfg(unix)]
fn read_loop(gate: &Gate, tx: &UnboundedSender<Input>) {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    use rustix::io::Errno;
    use std::os::fd::AsFd;

    let stdin = std::io::stdin();
    let fd = stdin.as_fd();
    let slice = Timespec {
        tv_sec: 0,
        tv_nsec: SLICE.as_nanos() as _,
    };
    let mut buf = [0u8; 4096];
    loop {
        if !gate.wait_open() {
            return;
        }
        let mut fds = [PollFd::new(&fd, PollFlags::IN)];
        match poll(&mut fds, Some(&slice)) {
            Ok(0) | Err(Errno::INTR) => continue,
            Ok(_) => {}
            Err(_) => return,
        }
        let state = gate.lock();
        if state.closed {
            return;
        }
        if state.paused {
            continue;
        }
        // poll 说可读：read 不阻塞，持锁读完。
        match rustix::io::read(fd, &mut buf) {
            // stdin 到头了（终端没了）：流结束。
            Ok(0) => return,
            Ok(n) => {
                if tx.send(Input::Bytes(buf[..n].to_vec())).is_err() {
                    return;
                }
            }
            Err(Errno::INTR | Errno::AGAIN) => {}
            Err(_) => return,
        }
        drop(state);
    }
}

/// windows 的读任务：读控制台事件，编码成字节（[`encode`]）。
#[cfg(windows)]
fn read_loop(gate: &Gate, tx: &UnboundedSender<Input>) {
    loop {
        if !gate.wait_open() {
            return;
        }
        match crossterm::event::poll(SLICE) {
            Ok(false) => continue,
            Ok(true) => {}
            Err(_) => return,
        }
        let state = gate.lock();
        if state.closed {
            return;
        }
        if state.paused {
            continue;
        }
        let Ok(event) = crossterm::event::read() else {
            return;
        };
        drop(state);
        if let Some(input) = encode(&event)
            && tx.send(input).is_err()
        {
            return;
        }
    }
}

/// 把 crossterm 的事件编码成 `KeyParser` 认得的字节（见模块文档）。认不出、不报告的事件（松开、悬停、单独的修饰键、
/// 功能键 F1–F12）是 `None`。
#[cfg(any(windows, test))]
pub(crate) fn encode(event: &crossterm::event::Event) -> Option<Input> {
    use crossterm::event::Event;
    let text = match event {
        Event::Key(key) => encode_key(key)?,
        Event::Mouse(m) => encode_mouse(*m)?,
        Event::Paste(s) => format!("\x1b[200~{s}\x1b[201~"),
        Event::FocusGained => "\x1b[I".to_string(),
        Event::FocusLost => "\x1b[O".to_string(),
        Event::Resize(..) => return Some(Input::Resize),
    };
    Some(Input::Bytes(text.into_bytes()))
}

/// kitty / xterm 的修饰参数：1 + shift 1、alt 2、ctrl 4、super 8。
#[cfg(any(windows, test))]
fn modifier_param(m: crossterm::event::KeyModifiers) -> u32 {
    use crossterm::event::KeyModifiers as K;
    let mut p = 1;
    for (flag, bit) in [(K::SHIFT, 1), (K::ALT, 2), (K::CONTROL, 4), (K::SUPER, 8)] {
        if m.contains(flag) {
            p += bit;
        }
    }
    p
}

#[cfg(any(windows, test))]
fn encode_key(key: &crossterm::event::KeyEvent) -> Option<String> {
    use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers as K};
    if key.kind == KeyEventKind::Release {
        return None;
    }
    let m = key.modifiers;
    let p = modifier_param(m);
    let csi_u = |code: u32| format!("\x1b[{code};{p}u");
    // 方向、Home、End：`CSI X` / `CSI 1;修饰 X`。
    let cursor = |x: char| {
        if m.is_empty() {
            format!("\x1b[{x}")
        } else {
            format!("\x1b[1;{p}{x}")
        }
    };
    // 编辑键：`CSI n ~` / `CSI n;修饰 ~`。
    let tilde = |n: u32| {
        if m.is_empty() {
            format!("\x1b[{n}~")
        } else {
            format!("\x1b[{n};{p}~")
        }
    };
    Some(match key.code {
        KeyCode::Char(c) => {
            // 只按着 Shift 的字已经是大写；Ctrl+Alt 加非字母数字的字是 AltGr 打出来的：都按文字送。
            let plain = !m.intersects(K::CONTROL | K::ALT | K::SUPER);
            let alt_gr = m.contains(K::CONTROL | K::ALT) && !c.is_ascii_alphanumeric();
            if plain || alt_gr {
                c.to_string()
            } else {
                // kitty 报的是不带 Shift 的键码。
                csi_u(u32::from(c.to_ascii_lowercase()))
            }
        }
        KeyCode::Enter if m.is_empty() => "\r".to_string(),
        KeyCode::Enter => csi_u(13),
        KeyCode::Tab if m.is_empty() => "\t".to_string(),
        KeyCode::Tab if m == K::SHIFT => "\x1b[Z".to_string(),
        KeyCode::Tab => csi_u(9),
        KeyCode::BackTab => "\x1b[Z".to_string(),
        KeyCode::Backspace if m.is_empty() => "\x7f".to_string(),
        KeyCode::Backspace => csi_u(127),
        KeyCode::Esc => csi_u(27),
        KeyCode::Up => cursor('A'),
        KeyCode::Down => cursor('B'),
        KeyCode::Right => cursor('C'),
        KeyCode::Left => cursor('D'),
        KeyCode::Home => cursor('H'),
        KeyCode::End => cursor('F'),
        KeyCode::Insert => tilde(2),
        KeyCode::Delete => tilde(3),
        KeyCode::PageUp => tilde(5),
        KeyCode::PageDown => tilde(6),
        _ => return None,
    })
}

/// SGR 1006：`CSI < 按键码;列;行 M|m`，列与行从 1 数起；修饰位 shift 4、alt 8、ctrl 16。
#[cfg(any(windows, test))]
fn encode_mouse(event: crossterm::event::MouseEvent) -> Option<String> {
    use crossterm::event::{KeyModifiers as K, MouseButton, MouseEventKind as E};
    let button = |b: MouseButton| match b {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    };
    let (code, press) = match event.kind {
        E::Down(b) => (button(b), true),
        E::Up(b) => (button(b), false),
        E::Drag(b) => (button(b) + 32, true),
        E::ScrollUp => (64, true),
        E::ScrollDown => (65, true),
        E::ScrollLeft => (66, true),
        E::ScrollRight => (67, true),
        E::Moved => return None,
    };
    let m = event.modifiers;
    let mods = [(K::SHIFT, 4), (K::ALT, 8), (K::CONTROL, 16)]
        .iter()
        .filter(|(flag, _)| m.contains(*flag))
        .map(|(_, bit)| bit)
        .sum::<u32>();
    Some(format!(
        "\x1b[<{};{};{}{}",
        code + mods,
        u32::from(event.column) + 1,
        u32::from(event.row) + 1,
        if press { 'M' } else { 'm' }
    ))
}

#[cfg(test)]
mod tests {
    use crossterm::event::{
        Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers as K, MouseButton, MouseEvent,
        MouseEventKind,
    };

    use super::{Input, Mux, encode, osc52};
    use crate::app::key::Key;
    use crate::app::key_parser::KeyParser;

    /// 编码后交给按键解析得到的键。
    fn keys(event: Event) -> Vec<Key> {
        match encode(&event) {
            Some(Input::Bytes(b)) => KeyParser::new().feed(&b),
            other => panic!("not bytes: {other:?}"),
        }
    }

    fn key(code: KeyCode, m: K) -> Event {
        Event::Key(KeyEvent::new(code, m))
    }

    #[test]
    fn windows_keys_decode_to_the_keys_the_app_binds() {
        assert_eq!(keys(key(KeyCode::Char('a'), K::NONE)), [Key::Char('a')]);
        assert_eq!(keys(key(KeyCode::Char('A'), K::SHIFT)), [Key::Char('A')]);
        assert_eq!(keys(key(KeyCode::Char('é'), K::NONE)), [Key::Char('é')]);
        assert_eq!(keys(key(KeyCode::Char('q'), K::CONTROL)), [Key::Ctrl('Q')]);
        assert_eq!(
            keys(key(KeyCode::Char('b'), K::ALT)),
            [Key::alt(Key::Char('b'))]
        );
        // AltGr（Ctrl+Alt）打出的字是文字。
        assert_eq!(
            keys(key(KeyCode::Char('@'), K::CONTROL | K::ALT)),
            [Key::Char('@')]
        );
        assert_eq!(keys(key(KeyCode::Enter, K::NONE)), [Key::Enter]);
        assert_eq!(keys(key(KeyCode::Enter, K::SHIFT)), [Key::Newline]);
        assert_eq!(keys(key(KeyCode::Enter, K::ALT)), [Key::alt(Key::Enter)]);
        assert_eq!(keys(key(KeyCode::Tab, K::NONE)), [Key::Tab]);
        assert_eq!(keys(key(KeyCode::BackTab, K::SHIFT)), [Key::BackTab]);
        assert_eq!(keys(key(KeyCode::Backspace, K::NONE)), [Key::Backspace]);
        assert_eq!(
            keys(key(KeyCode::Backspace, K::ALT)),
            [Key::alt(Key::Backspace)]
        );
        // Esc 不经 ESC 的歧义：立即是 Esc。
        assert_eq!(keys(key(KeyCode::Esc, K::NONE)), [Key::Esc]);
        assert_eq!(keys(key(KeyCode::Left, K::NONE)), [Key::Left]);
        assert_eq!(
            keys(key(KeyCode::Left, K::CONTROL)),
            [Key::with_ctrl(Key::Left)]
        );
        assert_eq!(keys(key(KeyCode::Home, K::NONE)), [Key::Home]);
        assert_eq!(keys(key(KeyCode::Delete, K::NONE)), [Key::Delete]);
        assert_eq!(keys(key(KeyCode::PageDown, K::NONE)), [Key::PageDown]);
    }

    #[test]
    fn windows_releases_and_function_keys_send_nothing() {
        let mut release = KeyEvent::new(KeyCode::Char('a'), K::NONE);
        release.kind = KeyEventKind::Release;
        assert_eq!(encode(&Event::Key(release)), None);
        assert_eq!(encode(&key(KeyCode::F(5), K::NONE)), None);
    }

    #[test]
    fn windows_mouse_paste_focus_and_resize() {
        let mouse = |kind, m| {
            Event::Mouse(MouseEvent {
                kind,
                column: 11,
                row: 4,
                modifiers: m,
            })
        };
        assert_eq!(
            keys(mouse(MouseEventKind::ScrollUp, K::NONE)),
            [Key::WheelUp { col: 12, row: 5 }]
        );
        assert_eq!(
            keys(mouse(MouseEventKind::Down(MouseButton::Left), K::SHIFT)),
            [Key::MousePress {
                button: 0,
                col: 12,
                row: 5,
                mods: 4
            }]
        );
        assert_eq!(
            keys(mouse(MouseEventKind::Up(MouseButton::Right), K::NONE)),
            [Key::MouseRelease {
                button: 2,
                col: 12,
                row: 5,
                mods: 0
            }]
        );
        assert_eq!(encode(&mouse(MouseEventKind::Moved, K::NONE)), None);
        assert_eq!(
            keys(Event::Paste("a\nb".into())),
            [Key::Paste("a\nb".into())]
        );
        assert_eq!(keys(Event::FocusGained), [Key::FocusIn]);
        assert_eq!(keys(Event::FocusLost), [Key::FocusOut]);
        assert_eq!(encode(&Event::Resize(1, 2)), Some(Input::Resize));
    }

    #[test]
    fn osc52_wraps_for_the_multiplexer() {
        // "hi" 的 base64 是 aGk=。
        assert_eq!(osc52("hi", Mux::None, false), "\x1b]52;c;aGk=\x07");
        assert_eq!(osc52("hi", Mux::None, true), "\x1b]52;c;aGk=\x1b\\");
        assert_eq!(
            osc52("hi", Mux::Tmux, false),
            "\x1b]52;c;aGk=\x07\x1bPtmux;\x1b\x1b]52;c;aGk=\x07\x1b\\"
        );
        assert_eq!(
            osc52("hi", Mux::Screen, false),
            "\x1bP\x1b]52;c;aGk=\x07\x1b\\"
        );
    }
}
