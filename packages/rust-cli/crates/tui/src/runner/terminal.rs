//! 真实终端：进入 alternate screen、读 crossterm 的事件流、用 OSC 52 复制（F25 起按 tmux / screen 包一层）；退出、出错都恢复终端。外部编辑器
//! 运行期间把终端交出去（tui-input.md §4.1）：先丢掉 crossterm 的事件流（它的读线程随之停下，按键留给编辑器），
//! 再恢复终端；收回时反向进行，事件流在下一次读时重建。
use rust_cli_base::sync::LockUnpoisoned;
use std::io::Write;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use anyhow::{Result, bail};
use crossterm::event::{Event, EventStream};
use futures_util::{Stream, StreamExt};
use rust_cli_kernel_api::session_api::Kernel;

use super::{Options, Screen, drive};
use crate::ui::console::Console;
use crate::ui::probe::{ColorLevel, Colors};
use crate::ui::terminal::Terminal;

/// 输入事件流的开关：交出终端时丢掉 crossterm 的事件流，收回后重建。
#[derive(Default)]
struct Gate {
    state: Mutex<GateState>,
}

#[derive(Default)]
struct GateState {
    paused: bool,
    stream: Option<EventStream>,
    waker: Option<Waker>,
}

impl Gate {
    fn lock(&self) -> std::sync::MutexGuard<'_, GateState> {
        self.state.lock_unpoisoned()
    }

    /// 同步丢掉事件流：之后编辑器启动，crossterm 不会再读走按键。
    fn pause(&self) {
        let mut state = self.lock();
        state.paused = true;
        state.stream = None;
    }

    fn resume(&self) {
        let mut state = self.lock();
        state.paused = false;
        if let Some(waker) = state.waker.take() {
            waker.wake();
        }
    }
}

/// 经 [`Gate`] 读的终端输入。
struct Input(Arc<Gate>);

impl Stream for Input {
    type Item = Event;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Event>> {
        let mut state = self.0.lock();
        if state.paused {
            state.waker = Some(cx.waker().clone());
            return Poll::Pending;
        }
        let stream = state.stream.get_or_insert_with(EventStream::new);
        loop {
            match stream.poll_next_unpin(cx) {
                Poll::Ready(Some(Ok(event))) => return Poll::Ready(Some(event)),
                // 读失败的事件跳过（与原来的 `filter_map(ok)` 相同）。
                Poll::Ready(Some(Err(_))) => {}
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// 写到 stdout 的终端副作用。
struct Stdout {
    guard: Terminal,
    gate: Arc<Gate>,
    /// 入口给的终端变量：OSC 52 在 tmux / screen 里要包一层（F25）。
    vars: std::collections::BTreeMap<String, String>,
    /// 进界面时探测到终端支持程序状态（OSC 7501，tui-manage.md §5.4）。
    program_status: bool,
}

impl Screen for Stdout {
    fn size(&self) -> std::io::Result<(u16, u16)> {
        self.guard.size()
    }

    fn write(&mut self, bytes: &str) -> std::io::Result<()> {
        self.guard.write(bytes)
    }

    fn keyboard_enhanced(&self) -> bool {
        self.guard.enhanced()
    }

    fn program_status(&self) -> bool {
        self.program_status
    }

    fn notify(&mut self, sequence: &str) {
        let mut out = std::io::stdout().lock();
        // 通知失败只影响这一次提醒。
        let _ = out.write_all(sequence.as_bytes());
        let _ = out.flush();
    }

    fn copy(&mut self, text: &str) {
        let sequence = crate::ui::clipboard::osc52(text, &self.vars);
        let mut out = std::io::stdout().lock();
        // 复制失败只影响这一次复制，不影响界面。
        let _ = out.write_all(sequence.as_bytes());
        let _ = out.flush();
    }

    fn suspend(&mut self) {
        self.gate.pause();
        self.guard.suspend();
    }

    fn resume(&mut self) {
        // 收回失败时终端停在交出的状态，下一帧照常画（画面可能错乱，但输入照常）。
        let _ = self.guard.reclaim();
        self.gate.resume();
    }
}

/// `zcode`（不带 `-p`）：全屏终端界面。返回前恢复终端（备用屏、鼠标、粘贴模式、键盘增强）；SessionEnd 还没跑，入口
/// 打印恢复提示之后调用 [`super::Ending::run`]（session-lifecycle.md §3.5）。
pub async fn run(kernel: Arc<dyn Kernel>, options: Options) -> Result<super::Exit> {
    // 不在终端里（stdin 或 stdout 是管道、文件）时读不到按键、也画不了屏幕：立即失败，不进入界面。Git Bash 的 mintty
    // 同样失败（工作线 F71，tui-input.md §9.6）。
    let preference = options.ui.locale.map(crate::services::UiLocale::as_str);
    let lang = crate::ui::i18n::resolve(preference, &options.vars);
    if let Some(refusal) = Console::current().refusal(crate::ui::i18n::strings(lang)) {
        bail!("{refusal}");
    }
    let (guard, probed) = crate::ui::terminal::enter()?;
    let colors = Colors::new(
        ColorLevel::detect(&options.vars, options.os),
        probed.background,
    )
    .with_foreground(probed.foreground);
    let gate = Arc::new(Gate::default());
    let input = Input(gate.clone());
    let mut screen = Stdout {
        guard,
        gate,
        vars: options.vars.clone(),
        program_status: probed.program_status,
    };
    let result = drive(kernel, options, colors, &mut screen, input).await;
    // 先恢复终端（`Terminal` 的 drop），再把 SessionEnd 交回入口。
    drop(screen);
    result
}
