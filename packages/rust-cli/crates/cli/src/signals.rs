//! 退出信号（docs/specs/rust-cli/session-lifecycle.md §3.5，R5-2）。
//!
//! 缺陷修复：原来入口不处理信号，SIGINT / SIGTERM / SIGHUP 按默认动作直接结束进程——Bash 的子进程留成孤儿，
//! TUI 停在备用屏，SessionEnd 与日志的收尾都被跳过。现在注册这些信号（优雅退出）：第一个到达时
//! 记下退出码（0 / 143 / 129）、取消 [`Stop::token`]；前端收到后走已有的退出路径（中止回合 → 进程树随工具的
//! 取消结束 → TUI 恢复终端与恢复提示 → SessionEnd，F14 起按这个顺序），入口再记 `process.exit`、flush 日志。
//! 之后的信号被吞掉：注册之后进程不再按默认动作结束。

use std::sync::{Arc, OnceLock};

use rust_cli_base::TaskScope;
use tokio_util::sync::CancellationToken;

/// 收到的第一个退出信号：名字与对应的退出码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Received {
    pub name: &'static str,
    pub code: u8,
}

impl Received {
    /// 用户中断（SIGINT；Windows 的 Ctrl+C、Ctrl+Break，D1）：`-p` 以 `user-cancel` 中止 query；其余信号
    /// 都不中止、直接退出（修复线 F38，session-lifecycle.md §3.5）。
    fn user_cancel(self) -> bool {
        self.code == 0
    }
}

/// 入口持有的监听：令牌交给前端，退出码在前端返回后读取。
pub(crate) struct Stop {
    pub token: CancellationToken,
    /// 用户中断的信号（[`Received::user_cancel`]）：先于 `token` 取消，`-p` 据此区分两种中止（F38）。
    pub cancel: CancellationToken,
    received: Arc<OnceLock<Received>>,
    /// 监听任务的作用域；释放时中止监听。
    _scope: TaskScope,
}

impl Stop {
    /// 注册信号并开始监听。注册失败（极少见）时只记警告：照旧按默认动作结束。
    pub(crate) fn listen() -> Self {
        let token = CancellationToken::new();
        let cancel = CancellationToken::new();
        let received = Arc::new(OnceLock::new());
        let scope = TaskScope::root("signals", |_| {});
        match listener() {
            Ok(first) => {
                let (token, cancel, received) = (token.clone(), cancel.clone(), received.clone());
                // 只等第一个：之后的信号仍被注册的处理器接住，不再结束进程。
                let _task = scope.spawner().spawn("signal-listener", async move {
                    let signal = first.await;
                    tracing::info!(
                        event = "process.signal",
                        signal = signal.name,
                        code = signal.code,
                        "exit signal received"
                    );
                    let _ = received.set(signal);
                    if signal.user_cancel() {
                        cancel.cancel();
                    }
                    token.cancel();
                });
            }
            Err(error) => tracing::warn!(
                event = "process.signal.unavailable",
                %error,
                "cannot listen for exit signals"
            ),
        }
        Self {
            token,
            cancel,
            received,
            _scope: scope,
        }
    }

    /// 收到过信号时它的退出码。
    pub(crate) fn code(&self) -> Option<u8> {
        self.received.get().map(|signal| signal.code)
    }
}

/// 注册全部信号，返回等第一个到达的 future（SIGINT → 0、SIGTERM → 143、SIGHUP → 129）。
#[cfg(unix)]
fn listener() -> std::io::Result<impl Future<Output = Received> + Send + 'static> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut hangup = signal(SignalKind::hangup())?;
    Ok(async move {
        let received = |name, code| Received { name, code };
        tokio::select! {
            _ = interrupt.recv() => received("SIGINT", 0),
            _ = terminate.recv() => received("SIGTERM", 143),
            _ = hangup.recv() => received("SIGHUP", 129),
        }
    })
}

/// Windows：Ctrl+C 与 Ctrl+Break 按 SIGINT（差异 D1）；控制台关闭、注销、关机按 SIGHUP
/// （Node 把控制台关闭报成 SIGHUP）。
#[cfg(windows)]
fn listener() -> std::io::Result<impl Future<Output = Received> + Send + 'static> {
    use tokio::signal::windows::{ctrl_break, ctrl_c, ctrl_close, ctrl_logoff, ctrl_shutdown};
    let mut interrupt = ctrl_c()?;
    let mut brk = ctrl_break()?;
    let mut close = ctrl_close()?;
    let mut logoff = ctrl_logoff()?;
    let mut shutdown = ctrl_shutdown()?;
    Ok(async move {
        let received = |name, code| Received { name, code };
        tokio::select! {
            _ = interrupt.recv() => received("CTRL_C", 0),
            _ = brk.recv() => received("CTRL_BREAK", 0),
            _ = close.recv() => received("CTRL_CLOSE", 129),
            _ = logoff.recv() => received("CTRL_LOGOFF", 129),
            _ = shutdown.recv() => received("CTRL_SHUTDOWN", 129),
        }
    })
}
