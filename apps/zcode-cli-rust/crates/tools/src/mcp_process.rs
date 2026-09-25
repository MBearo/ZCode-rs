//! Crash detection of a stdio MCP server (Node `client.onclose` with the
//! process exit, spec rust-m9-usage-logs §6): the server's stdout ends, and
//! unless the connection is closing on purpose, an exited process is a crash.
use super::mcp_telemetry::Tracker;
use futures_util::Stream;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::process::Child;
use tokio::sync::{Mutex, oneshot};

/// How long an ended stdout waits for the process exit (Node checks liveness at close).
const EXIT_GRACE: Duration = Duration::from_secs(1);

/// A stream that signals once when it ends.
pub(super) struct EndSignal<S> {
    inner: S,
    end: Option<oneshot::Sender<()>>,
}

impl<S> EndSignal<S> {
    pub fn new(inner: S) -> (Self, oneshot::Receiver<()>) {
        let (end, ended) = oneshot::channel();
        (
            Self {
                inner,
                end: Some(end),
            },
            ended,
        )
    }
}

impl<S: Stream + Unpin> Stream for EndSignal<S> {
    type Item = S::Item;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<S::Item>> {
        let next = Pin::new(&mut self.inner).poll_next(cx);
        if let Poll::Ready(None) = next
            && let Some(end) = self.end.take()
        {
            let _ = end.send(());
        }
        next
    }
}

/// Node's signal names of a Unix exit.
#[cfg(unix)]
fn signal_name(signal: i32) -> String {
    let name = match signal {
        1 => "SIGHUP",
        2 => "SIGINT",
        3 => "SIGQUIT",
        4 => "SIGILL",
        6 => "SIGABRT",
        8 => "SIGFPE",
        9 => "SIGKILL",
        10 => "SIGUSR1",
        11 => "SIGSEGV",
        12 => "SIGUSR2",
        13 => "SIGPIPE",
        14 => "SIGALRM",
        15 => "SIGTERM",
        _ => return format!("SIG{signal}"),
    };
    name.into()
}

/// The process of one connection and what its end means.
pub(super) struct Watch {
    pub tracker: Arc<Tracker>,
    pub instance: String,
    pub closing: Arc<AtomicBool>,
}

/// Waits briefly for the process to exit and records it as a crash; a
/// process still alive is not one (Node checks liveness).
pub(super) async fn report_exit(tracker: &Tracker, instance: &str, child: &mut Child) {
    let Ok(Ok(status)) = tokio::time::timeout(EXIT_GRACE, child.wait()).await else {
        return;
    };
    #[cfg(unix)]
    let signal = std::os::unix::process::ExitStatusExt::signal(&status).map(signal_name);
    #[cfg(not(unix))]
    let signal: Option<String> = None;
    tracker.crashed(instance, status.code(), signal.as_deref());
}

impl Watch {
    /// Waits for stdout to end; an exit that no `close` asked for is a crash.
    pub fn spawn(self, ended: oneshot::Receiver<()>, child: Arc<Mutex<Option<(Child, u32)>>>) {
        tokio::spawn(async move {
            if ended.await.is_err() || self.closing.load(Ordering::SeqCst) {
                return;
            }
            let mut guard = child.lock().await;
            if self.closing.load(Ordering::SeqCst) {
                return;
            }
            if let Some((child, _)) = guard.as_mut() {
                report_exit(&self.tracker, &self.instance, child).await;
            }
        });
    }
}
