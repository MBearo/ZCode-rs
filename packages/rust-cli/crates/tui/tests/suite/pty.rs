#![allow(
    clippy::disallowed_methods,
    reason = "伪终端的测试把这个测试二进制自己作为子进程起在从端上"
)]
//! 伪终端（只在 unix 上）：测试打开一个伪终端，把这个测试二进制自己作为子进程起在从端上（stdin、stdout、stderr 都是
//! 它），子进程进入真实终端。父进程读主端的输出、回答主要设备属性的查询，读从端的 termios 判断 raw mode。
//! `panic_terminal.rs`（修复线 F109）与 `find_pty.rs`（修复线 F114）共用。
use std::collections::BTreeMap;
use std::os::fd::OwnedFd;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::model::ModelPort;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::{
    MemoryStore, SequentialIds, UnusedEnvironment, environments, selection,
};
use rust_cli_tui::Options;
use rustix::termios::{LocalModes, Winsize, tcgetattr};
use tokio_util::sync::CancellationToken;

use super::support::{WallClock, kernel};

/// 子进程按这个环境变量扮演哪一种情况。
pub const ROLE: &str = "RUST_CLI_PTY_TEST_ROLE";

/// 画面上有没有 `needle`：去掉控制序列与空白再比（空格不画，光标直接跳过去）。
pub fn shows(text: &str, needle: &str) -> bool {
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

/// 伪终端的主端、从端与起在从端上的子进程（这个测试二进制自己），以及读到的全部输出。
pub struct Pty {
    master: OwnedFd,
    slave: OwnedFd,
    child: Child,
    seen: Vec<u8>,
    answered: usize,
    /// 已经回答过的光标位置查询（`ESC [ 6 n`）个数。
    positions: usize,
}

impl Pty {
    /// 打开伪终端，把测试 `child`（全名，例如 `suite::panic_terminal::child`）作为子进程起在从端上，环境变量 [`ROLE`] 是
    /// `role`。
    pub fn start(child: &str, role: &str) -> Self {
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
            .args(["--exact", child, "--ignored", "--nocapture"])
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
            positions: 0,
        }
    }

    /// 读输出直到 `done` 为真（最多 20 秒），返回到那时为止的全部输出。
    pub fn until(&mut self, what: &str, done: impl Fn(&str) -> bool) -> String {
        self.until_from(0, what, done)
    }

    /// 到现在为止读到的字节数（[`Self::until_from`] 的起点）。
    pub fn mark(&self) -> usize {
        self.seen.len()
    }

    /// 同 [`Self::until`]，只看第 `from` 个字节之后的输出（终端只重画变了的格子，之前的帧里出现过的字不算）；返回这一段。
    pub fn until_from(&mut self, from: usize, what: &str, done: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let tail = &self.seen[from.min(self.seen.len())..];
            let text = String::from_utf8_lossy(tail).into_owned();
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
    pub fn finish(&mut self) -> (ExitStatus, String) {
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

    /// 读一次；读到了返回真。新出现的主要设备属性查询（`ESC [ c`）照真实终端回答，进入终端时的探测不用等超时；光标位置
    /// 查询（`ESC [ 6 n`，ctrl+l 整屏重画时清屏要问）回答左上角（修复线 F117 的 `background_pty.rs` 用 ctrl+l）。
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
        let positions = self
            .seen
            .windows(4)
            .filter(|window| *window == b"\x1b[6n")
            .count();
        for _ in self.positions..positions {
            self.send(b"\x1b[1;1R");
        }
        self.positions = positions;
        true
    }

    pub fn send(&self, bytes: &[u8]) {
        rustix::io::write(&self.master, bytes).unwrap();
    }

    pub fn raw(&self) -> bool {
        is_raw(&self.slave)
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn is_raw(fd: impl std::os::fd::AsFd) -> bool {
    !tcgetattr(fd)
        .unwrap()
        .local_modes
        .contains(LocalModes::ICANON)
}

/// 子进程里：在真实终端上跑会话界面（英文，16 色：只有 `LANG`），模型是 `model`。
pub async fn interface(model: Arc<dyn ModelPort>) {
    interface_in(model, environments(Arc::new(UnusedEnvironment))).await;
}

/// 同 [`interface`]，执行环境是 `envs`（修复线 F117 的 `background_pty.rs`：替身报几条在跑的后台命令）。
pub async fn interface_in(
    model: Arc<dyn ModelPort>,
    envs: Arc<dyn rust_cli_kernel_api::env_registry::EnvironmentRegistry>,
) {
    let kernel = kernel(
        model,
        envs,
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
