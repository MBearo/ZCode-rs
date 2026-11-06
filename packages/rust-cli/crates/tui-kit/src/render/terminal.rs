//! 终端的接缝：原始模式、输入字节、尺寸，与每帧一次的写。字节到按键的解码在
//! 运行时里（`KeyParser`，纯函数），这里只搬字节。
//! 移植自 packages/tui/tui/src/zcode/tui/render/Terminal.scala（feat/tui a2bf878b34）。
//!
//! 与 Scala 的不同：
//!
//! - 输入是拉取的流（[`Terminal::input`]，只取一次），不是注册回调（`onByte`、`onResize`）：字节与 resize 是同一个流
//!   的两种 [`Input`]，运行时在自己的循环里读它。
//! - 帧之外的写（铃、标题、OSC 52 复制）经 [`Terminal::signals`] 的句柄：Scala 把 `terminal.write` 与
//!   `copyToClipboard` 直接交给壳，在 JS 的单线程里调用；这里终端归运行时的循环所有，壳的 `Cmd::Fire` 在别的任务里
//!   跑，所以给它一个可克隆、线程安全的句柄。真实终端的句柄与帧写同一个 stdout，每次写在 stdout 的锁里整段写出。
//! - [`Terminal::enter_raw`] 返回错误（Scala 抛异常）：进不了原始模式时运行时不启动。其余步骤尽力而为，不报错。
use std::io::Write;
use std::sync::Arc;

use futures_util::stream::{self, BoxStream, StreamExt};

/// 终端送来的东西：原始模式下的输入字节，或终端尺寸变了（OS 的 resize 信号）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    Bytes(Vec<u8>),
    Resize,
}

/// 帧之外写到终端的东西：铃、窗口标题、终端通知、OSC 52 复制。壳的效果在别的任务里调用它，所以要 `Send + Sync`。
pub trait Signals: Send + Sync {
    /// 原样写出（一次写）。终端关闭后什么也不做。
    fn write(&self, s: &str);
    /// 把 `text` 放进系统剪贴板（经终端）。
    fn copy_to_clipboard(&self, text: &str);
}

/// 什么也不做的 [`Signals`]。
pub struct NoSignals;

impl Signals for NoSignals {
    fn write(&self, _: &str) {}
    fn copy_to_clipboard(&self, _: &str) {}
}

/// 终端 IO 的接缝。具体实现：真实的 TTY（`render::real::RealTerminal`）、没有 TTY 时的 [`HeadlessTerminal`]、测试的
/// 假终端（`testing::fake_terminal`）。
pub trait Terminal: Send {
    /// 原样写出一帧（并 flush）：一次调用一次写。
    fn write(&mut self, s: &str);

    /// 现在的 `(列, 行)`。
    fn size(&self) -> (usize, usize);

    /// 输入字节与 resize 的流。只取一次：之后再取得到空流。
    fn input(&mut self) -> BoxStream<'static, Input>;

    /// 进入原始模式（关回显、行缓冲与信号键），并打开本终端要的输入模式。
    fn enter_raw(&mut self) -> std::io::Result<()>;
    fn leave_raw(&mut self);

    fn hide_cursor(&mut self);
    fn show_cursor(&mut self);

    /// 打开 / 关闭鼠标报告。缺省什么也不做：只有真实终端覆盖它们。
    fn enable_mouse(&mut self) {}
    fn disable_mouse(&mut self) {}

    /// 把 `text` 放进系统剪贴板。缺省什么也不做；真实终端写 OSC 52（复用器里包一层）。
    fn copy_to_clipboard(&mut self, _text: &str) {}

    /// 帧之外的写的句柄（见模块文档）。缺省是 [`NoSignals`]。
    fn signals(&self) -> Arc<dyn Signals> {
        Arc::new(NoSignals)
    }

    /// 渲染器紧跟在进入备用屏（`?1049h`）之后写的字节，与紧挨在离开（`?1049l`）之前写的字节。缺省都为空。
    ///
    /// kitty 键盘协议的增强栈按屏幕缓冲记：主屏上的 push（见真实终端的 `enter_raw`）不带进备用屏。取文字输入的全屏视图
    /// 在那里会退回传统的按键报告——Shift+Enter 成了裸的 CR，而不是 `CSI 13;2u`——所以进入时再 push、离开时 pop，两块
    /// 缓冲的栈各自配平。不是 kitty 的终端与无头终端为空，它们的写出流不变。
    fn alt_screen_setup(&self) -> String {
        String::new()
    }
    fn alt_screen_teardown(&self) -> String {
        String::new()
    }

    /// 把终端交给子程序（`Cmd::Handoff`）：输入不再到达 [`Terminal::input`] 的流，原始模式与本终端打开的输入模式都关上。
    /// 缺省什么也不做。
    fn suspend(&mut self) {}

    /// [`Terminal::suspend`] 之后收回终端：原始模式、输入模式与输入流都与之前一样。缺省什么也不做。
    fn resume(&mut self) {}

    /// 还原终端（离开原始模式、显示光标）。幂等。
    fn close(&mut self);
}

/// 没有控制终端时（管道、CI）的终端：没有输入、固定 80×24、写到 stdout。应用在非交互的场合也能跑而不崩溃。
#[derive(Clone, Copy, Debug, Default)]
pub struct HeadlessTerminal;

impl Terminal for HeadlessTerminal {
    fn write(&mut self, s: &str) {
        let mut out = std::io::stdout().lock();
        // 写不出去（管道关了）只丢掉这一帧。
        let _ = out.write_all(s.as_bytes());
        let _ = out.flush();
    }

    fn size(&self) -> (usize, usize) {
        (80, 24)
    }

    fn input(&mut self) -> BoxStream<'static, Input> {
        stream::pending().boxed()
    }

    fn enter_raw(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    fn leave_raw(&mut self) {}

    fn hide_cursor(&mut self) {}

    fn show_cursor(&mut self) {}

    fn close(&mut self) {}
}
