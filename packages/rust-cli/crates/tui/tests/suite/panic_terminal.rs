#![allow(
    clippy::disallowed_methods,
    reason = "测试把自己作为子进程起在伪终端上，子进程按环境变量扮演哪一种情况"
)]
//! panic 与终端（修复线 F109，docs/specs/rust-cli/tui.md §7.9）：测试打开一个伪终端，把这个测试二进制自己作为子进程
//! 起在从端上（stdin、stdout、stderr 都是它），子进程进入真实终端。父进程读主端的输出、回答主要设备属性的查询，
//! 读从端的 termios 判断 raw mode。
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::stream::BoxStream;
use rust_cli_kernel_api::model::{ModelPort, ModelRequest};
use rust_cli_kernel_api::testing::text_response;
use rust_cli_tui::ui::terminal::enter;
use tokio_util::sync::CancellationToken;

use super::pty::{Pty, ROLE, interface, is_raw, shows};
use super::support::{GatedModel, Item};

/// 子进程的入口（[`child`]）。
const CHILD: &str = "suite::panic_terminal::child";
/// 离开 alternate screen。
const LEAVE: &str = "\x1b[?1049l";

/// 被 base 的任务捕获的 panic：终端留在界面里（raw mode、alternate screen），屏幕上没有 panic 的文字，日志里有。
#[test]
fn a_caught_panic_leaves_the_terminal_as_it_is() {
    let mut pty = Pty::start(CHILD, "caught");
    let shown = pty.until("the marker", |text| text.contains("MARK-END"));
    let before = &shown[..shown.find("MARK raw=").unwrap()];
    assert!(shown.contains("MARK raw=true"), "{shown:?}");
    assert!(!before.contains(LEAVE), "{shown:?}");
    assert!(shown.contains("process.panic"), "{shown:?}");
    assert!(shown.contains("panic_terminal.rs:"), "{shown:?}");
    assert!(shown.contains("caught in a task"), "{shown:?}");
    let (status, all) = pty.finish();
    assert!(status.success(), "{all:?}");
    assert!(!all.contains("panicked at"), "{all:?}");
    // 正常退出时 Guard 照常恢复。
    assert!(all.contains(LEAVE), "{all:?}");
    assert!(!pty.raw());
}

/// 退出型 panic（持有 Guard 的线程）：栈展开时先恢复终端，再把 panic 的文字写到 stderr，只写一次。
#[test]
fn an_exit_panic_restores_the_terminal_before_the_text() {
    let mut pty = Pty::start(CHILD, "exit");
    let (status, all) = pty.finish();
    assert!(!status.success(), "{all:?}");
    let leave = all.find(LEAVE).unwrap_or_else(|| panic!("{all:?}"));
    let text = all.find("panicked at").unwrap_or_else(|| panic!("{all:?}"));
    assert!(leave < text, "{all:?}");
    assert!(all.contains("panic_terminal.rs:"), "{all:?}");
    assert_eq!(all.matches("exit path panic").count(), 1, "{all:?}");
    assert!(!pty.raw());
}

/// 展开中又 panic（随后 abort，Guard 等不到）：hook 恢复终端，两条文字都写出。
#[test]
fn a_panic_during_unwinding_still_restores_the_terminal() {
    let mut pty = Pty::start(CHILD, "double");
    let (status, all) = pty.finish();
    assert!(!status.success(), "{all:?}");
    let leave = all.find(LEAVE).unwrap_or_else(|| panic!("{all:?}"));
    let first = all.find("first panic").unwrap_or_else(|| panic!("{all:?}"));
    let second = all
        .find("second panic")
        .unwrap_or_else(|| panic!("{all:?}"));
    assert!(leave < first && first < second, "{all:?}");
    assert!(!pty.raw());
}

/// 端到端：模型替身第一次请求就 panic（与工具 panic 同在 query 的任务里被捕获），回合失败；界面仍在 raw mode 与
/// alternate screen 里，屏幕上没有 panic 的文字，下一条输入照常得到回复，`/exit` 之后终端恢复。
#[test]
fn the_interface_keeps_working_after_a_panic_in_a_turn() {
    let mut pty = Pty::start(CHILD, "session");
    pty.until("the interface", |text| shows(text, "Ask before changes"));
    pty.send(b"go\r");
    let failed = pty.until("the failed turn", |text| {
        shows(text, "simulated model failure")
    });
    assert!(pty.raw(), "{failed:?}");
    assert!(!failed.contains(LEAVE), "{failed:?}");
    assert!(!shows(&failed, "panicked at"), "{failed:?}");
    pty.send(b"again\r");
    let replied = pty.until("the reply", |text| shows(text, "survived"));
    assert!(pty.raw(), "{replied:?}");
    assert!(!replied.contains(LEAVE), "{replied:?}");
    pty.send(b"/exit");
    pty.until("the command", |text| shows(text, "Exit the CLI"));
    pty.send(b"\r");
    let (status, all) = pty.finish();
    assert!(status.success(), "{all:?}");
    assert!(all.contains(LEAVE), "{all:?}");
    assert!(!shows(&all, "panicked at"), "{all:?}");
    assert!(!pty.raw());
}

// ---- 子进程 ----

/// 子进程的入口（只被上面的测试经伪终端启动）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "被伪终端的测试作为子进程启动"]
async fn child() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    match role.as_str() {
        "caught" => caught().await,
        "exit" => {
            let _entered = enter().unwrap();
            panic!("exit path panic");
        }
        "double" => {
            let _entered = enter().unwrap();
            let _bomb = Bomb;
            panic!("first panic");
        }
        "session" => session().await,
        other => panic!("unknown role {other}"),
    }
}

/// 工作线程上的任务 panic、被 base 捕获之后，报告终端是否仍在 raw mode 与记下的日志。
async fn caught() {
    let logs = Arc::new(Mutex::new(Vec::new()));
    tracing::subscriber::set_global_default(Capture(logs.clone())).unwrap();
    let entered = enter().unwrap();
    let task = rust_cli_base::owned(async {
        if true {
            panic!("caught in a task");
        }
        1
    });
    assert!(task.await.is_err());
    let raw = is_raw(std::io::stdin());
    let logs = logs.lock().unwrap().clone();
    let mark = format!("\r\nMARK raw={raw} log={logs:?}\r\nMARK-END\r\n");
    std::io::stdout().write_all(mark.as_bytes()).unwrap();
    std::io::stdout().flush().unwrap();
    drop(entered);
}

/// 真实终端里的会话界面：第一次请求 panic，第二次回复 `survived`。
async fn session() {
    let model = Arc::new(PanicsOnce {
        tripped: AtomicBool::new(false),
        then: GatedModel::scripted(vec![text_response(&["survived"])]),
    });
    interface(model).await;
}

/// 第一次请求时 panic，之后交给 `then`。
struct PanicsOnce {
    tripped: AtomicBool,
    then: Arc<GatedModel>,
}

impl ModelPort for PanicsOnce {
    fn stream(&self, request: ModelRequest, cancel: CancellationToken) -> BoxStream<'static, Item> {
        assert!(
            self.tripped.swap(true, Ordering::SeqCst),
            "simulated model failure"
        );
        self.then.stream(request, cancel)
    }
}

/// 析构时 panic：在栈展开中再 panic 一次。
struct Bomb;

impl Drop for Bomb {
    fn drop(&mut self) {
        panic!("second panic");
    }
}

/// 记下每个事件的字段。
struct Capture(Arc<Mutex<Vec<String>>>);

impl tracing::Subscriber for Capture {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let mut fields = Fields(String::new());
        event.record(&mut fields);
        self.0.lock().unwrap().push(fields.0);
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}

struct Fields(String);

impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0 = format!("{}{}={value:?} ", self.0, field.name());
    }
}
