#![allow(
    clippy::disallowed_methods,
    reason = "测试把自己作为子进程起在伪终端上，子进程按环境变量扮演哪一种情况"
)]
//! panic 与终端（修复线 F109，docs/specs/rust-cli/tui.md §7.9）：测试打开一个伪终端，把这个测试二进制自己作为子进程
//! 起在从端上（stdin、stdout、stderr 都是它），子进程进入真实终端。父进程读主端的输出、回答主要设备属性的查询，
//! 读从端的 termios 判断 raw mode。
use std::collections::BTreeMap;
use std::io::Write;
use std::os::fd::OwnedFd;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::stream::BoxStream;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::model::{ModelPort, ModelRequest};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::{
    MemoryStore, SequentialIds, UnusedEnvironment, environments, selection, text_response,
};
use rust_cli_tui::Options;
use rust_cli_tui::ui::terminal::enter;
use rustix::termios::{LocalModes, Winsize, tcgetattr};
use tokio_util::sync::CancellationToken;

use super::support::{GatedModel, Item, WallClock, kernel};

const ROLE: &str = "RUST_CLI_PANIC_TERMINAL_ROLE";
/// 离开 alternate screen。
const LEAVE: &str = "\x1b[?1049l";

/// 被 base 的任务捕获的 panic：终端留在界面里（raw mode、alternate screen），屏幕上没有 panic 的文字，日志里有。
#[test]
fn a_caught_panic_leaves_the_terminal_as_it_is() {
    let mut pty = Pty::start("caught");
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
    let mut pty = Pty::start("exit");
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
    let mut pty = Pty::start("double");
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
    let mut pty = Pty::start("session");
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

/// 画面上有没有 `needle`：去掉控制序列与空白再比（空格不画，光标直接跳过去）。
fn shows(text: &str, needle: &str) -> bool {
    let squash = |text: &str| {
        let mut out = String::new();
        let mut chars = text.chars();
        while let Some(ch) = chars.next() {
            match ch {
                // CSI：参数之后的终止字节在 `@`..=`~`；OSC：到 BEL 或 ST。
                '\x1b' => match chars.next() {
                    Some('[') => while chars.next().is_some_and(|ch| !('@'..='~').contains(&ch)) {},
                    Some(']') => while chars.next().is_some_and(|ch| ch != '\x07' && ch != '\\') {},
                    _ => {}
                },
                ch if ch.is_whitespace() => {}
                ch => out.push(ch),
            }
        }
        out
    };
    squash(text).contains(&squash(needle))
}

// ---- 父进程：伪终端与子进程 ----

struct Pty {
    master: OwnedFd,
    slave: OwnedFd,
    child: Child,
    seen: Vec<u8>,
    answered: usize,
}

impl Pty {
    fn start(role: &str) -> Self {
        use rustix::fs::{Mode, OFlags};
        use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
        let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
        grantpt(&master).unwrap();
        unlockpt(&master).unwrap();
        let name = ptsname(&master, Vec::new()).unwrap();
        let slave = rustix::fs::open(
            name.as_c_str(),
            OFlags::RDWR | OFlags::NOCTTY,
            Mode::empty(),
        )
        .unwrap();
        let size = Winsize {
            ws_row: 30,
            ws_col: 100,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        rustix::termios::tcsetwinsize(&slave, size).unwrap();
        let io = || Stdio::from(slave.try_clone().unwrap());
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "suite::panic_terminal::child",
                "--ignored",
                "--nocapture",
            ])
            .env(ROLE, role)
            .stdin(io())
            .stdout(io())
            .stderr(io())
            .spawn()
            .unwrap();
        Self {
            master,
            slave,
            child,
            seen: Vec::new(),
            answered: 0,
        }
    }

    /// 读输出直到 `done` 为真（最多 20 秒），返回到那时为止的全部输出。
    fn until(&mut self, what: &str, done: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let text = String::from_utf8_lossy(&self.seen).into_owned();
            if done(&text) {
                return text;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}: {text:?}"
            );
            self.read(Duration::from_millis(100));
        }
    }

    /// 等子进程退出，再读完剩下的输出（从端还开着，读不到文件结束，读到停顿为止）。
    fn finish(&mut self) -> (ExitStatus, String) {
        let deadline = Instant::now() + Duration::from_secs(20);
        let status = loop {
            self.read(Duration::from_millis(50));
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "the child did not exit");
        };
        while self.read(Duration::from_millis(300)) {}
        (status, String::from_utf8_lossy(&self.seen).into_owned())
    }

    /// 读一次；读到了返回真。新出现的主要设备属性查询（`ESC [ c`）照真实终端回答，进入终端时的探测不用等超时。
    fn read(&mut self, wait: Duration) -> bool {
        use rustix::event::{PollFd, PollFlags, Timespec, poll};
        let wait = Timespec {
            tv_sec: wait.as_secs() as _,
            tv_nsec: wait.subsec_nanos() as _,
        };
        let mut fds = [PollFd::new(&self.master, PollFlags::IN)];
        if !matches!(poll(&mut fds, Some(&wait)), Ok(n) if n > 0) {
            return false;
        }
        let mut chunk = [0u8; 4096];
        let Ok(read @ 1..) = rustix::io::read(&self.master, &mut chunk) else {
            return false;
        };
        self.seen.extend_from_slice(&chunk[..read]);
        let queries = self
            .seen
            .windows(3)
            .filter(|window| *window == b"\x1b[c")
            .count();
        for _ in self.answered..queries {
            self.send(b"\x1b[?62c");
        }
        self.answered = queries;
        true
    }

    fn send(&self, bytes: &[u8]) {
        rustix::io::write(&self.master, bytes).unwrap();
    }

    fn raw(&self) -> bool {
        is_raw(&self.slave)
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn is_raw(fd: impl std::os::fd::AsFd) -> bool {
    !tcgetattr(fd)
        .unwrap()
        .local_modes
        .contains(LocalModes::ICANON)
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
    let kernel = kernel(
        model,
        environments(Arc::new(UnusedEnvironment)),
        Arc::new(MemoryStore::default()),
        None,
        rust_cli_runtime::PeerDeps::off(),
        None,
        None,
    );
    let options = Options {
        history: Vec::new(),
        recorder: None,
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/Users/dev/project"),
        },
        home: Some("/Users/dev".into()),
        selection: selection(),
        mode: PermissionMode::Default,
        version: "0.1.0".into(),
        vars: BTreeMap::from([("LANG".to_owned(), "en_US.UTF-8".to_owned())]),
        models: None,
        startup: rust_cli_tui::Startup::New,
        fork: false,
        services: rust_cli_tui::Services::default(),
        stop: CancellationToken::new(),
        clock: Arc::new(WallClock),
        ids: Arc::new(SequentialIds::default()),
        os: rust_cli_tui::Os::Unix,
    };
    rust_cli_tui::run(kernel, options).await.unwrap();
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
