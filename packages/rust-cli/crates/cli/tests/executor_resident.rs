//! 常驻执行器，真实进程（docs/specs/rust-cli/remote-exec.md §19.3、§19.5、§19.7、§19.12）：起停、`status` / `stop`、
//! 两个代理共用一个常驻进程、杀掉代理后命令继续跑并经重连取到输出、杀掉常驻进程后重启并把没有终态的调用记成中断。
//!
//! 每个测试用自己的临时 HOME（socket 与账本都在里面）；结束时 `zcode executor stop`，不留常驻进程。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录、拉起执行器与代理进程、发信号"
)]

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use rust_cli_env_remote::RemoteEnvironment;
use rust_cli_exec_client::{ClientOptions, ExecClient, Reconnect};
use rust_cli_exec_transport::stream::stream_link;
use rust_cli_exec_transport::{Connector, Link, LinkError, MAX_FRAME};
use rust_cli_kernel_api::env::{
    CommandEvent, EnvError, EnvPath, OperationContext, Origin, StartCommand, WorkspaceRef,
    WriteFile, WritePrecondition,
};
use rust_cli_kernel_api::env_shell::CwdScope;
use rust_cli_kernel_api::ids::{
    EnvironmentId, Generation, OperationId, OwnerEpoch, SessionId, ShellTaskId,
};
use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_zcode-rs");

/// 一个临时的用户环境；丢掉时停掉它的常驻执行器。
struct Home {
    home: tempfile::TempDir,
    work: PathBuf,
    _work: tempfile::TempDir,
}

impl Home {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        Self {
            work: std::fs::canonicalize(work.path()).unwrap(),
            home,
            _work: work,
        }
    }

    fn env(&self) -> Vec<(String, String)> {
        let home = self.home.path().to_string_lossy().into_owned();
        vec![
            ("HOME".to_owned(), home.clone()),
            ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
            ("SHELL".to_owned(), "/bin/bash".to_owned()),
            ("TMPDIR".to_owned(), home),
        ]
    }

    fn command(&self, args: &[&str]) -> std::process::Command {
        let mut command = std::process::Command::new(BIN);
        command
            .args(args)
            .env_clear()
            .envs(self.env())
            .current_dir(&self.work);
        command
    }

    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let output = self.command(args).output().unwrap();
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    fn status(&self) -> Value {
        let (_, stdout, _) = self.run(&["executor", "status", "--json"]);
        serde_json::from_str(stdout.trim()).unwrap()
    }

    fn exec_dir(&self) -> PathBuf {
        self.home.path().join(".zcode/rust-cli/exec")
    }

    fn workspace(&self) -> WorkspaceRef {
        WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new(self.work.to_string_lossy()),
        }
    }

    /// 等常驻进程的状态满足条件（测试里的等待；产品代码不轮询）。
    async fn wait_for(&self, what: &str, check: impl Fn(&Value) -> bool) -> Value {
        for _ in 0..400 {
            let status = self.status();
            if check(&status) {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("timed out waiting for {what}: {}", self.status());
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = self.command(&["executor", "stop"]).output();
    }
}

/// 每次连接拉起一个 `zcode executor --stdio` 代理，记下它的 pid（测试要杀它）。
struct Proxies {
    home_env: Vec<(String, String)>,
    cwd: PathBuf,
    pids: Mutex<Vec<u32>>,
    children: Mutex<Vec<tokio::process::Child>>,
}

impl Proxies {
    fn new(home: &Home) -> Arc<Self> {
        Arc::new(Self {
            home_env: home.env(),
            cwd: home.work.clone(),
            pids: Mutex::default(),
            children: Mutex::default(),
        })
    }
    fn last(&self) -> u32 {
        *self.pids.lock().unwrap().last().unwrap()
    }

    fn count(&self) -> usize {
        self.pids.lock().unwrap().len()
    }
}

#[async_trait]
impl Connector for Proxies {
    async fn connect(&self) -> Result<Box<dyn Link>, LinkError> {
        let mut child = tokio::process::Command::new(BIN)
            .args(["executor", "--stdio"])
            .env_clear()
            .envs(self.home_env.clone())
            .current_dir(&self.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| LinkError::Connect(error.to_string()))?;
        self.pids.lock().unwrap().push(child.id().unwrap());
        let link = stream_link(
            child.stdout.take().unwrap(),
            child.stdin.take().unwrap(),
            "proxy",
            MAX_FRAME,
        );
        self.children.lock().unwrap().push(child);
        Ok(Box::new(link))
    }

    fn describe(&self) -> String {
        "test proxy".to_owned()
    }
}

async fn client(proxies: &Arc<Proxies>, instance: &str) -> ExecClient {
    let mut options = ClientOptions::new("test", instance);
    options.reconnect = Reconnect {
        attempts: 20,
        delay: Duration::from_millis(100),
        max_delay: Duration::from_millis(100),
    };
    ExecClient::connect(proxies.clone(), options).await.unwrap()
}

fn context(operation: &str) -> OperationContext {
    OperationContext {
        environment: EnvironmentId::new("local"),
        generation: Generation::new("g"),
        owner_epoch: OwnerEpoch::LOCAL,
        operation: OperationId::new(operation),
        origin: Origin::User {
            session: Some(SessionId::new("s1")),
        },
    }
}

fn command(root: &Path, task: &str, text: &str) -> StartCommand {
    StartCommand {
        command: text.to_owned(),
        cwd: EnvPath::new(root.to_string_lossy()),
        timeout_ms: Some(30_000),
        task: ShellTaskId::new(task),
        shell: Default::default(),
        cwd_scope: CwdScope::Pinned,
        background: None,
        sandbox: None,
        hook: None,
        on_timeout: None,
        socket: None,
    }
}

fn kill(pid: u32) {
    let status = std::process::Command::new("/bin/kill")
        .args(["-9", &pid.to_string()])
        .status()
        .unwrap();
    assert!(status.success());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_resident_starts_reports_its_status_and_stops() {
    let home = Home::new();
    let (code, stdout, _) = home.run(&["executor", "status"]);
    assert_eq!((code, stdout.trim()), (3, "zcode executor is not running"));
    assert_eq!(home.status()["resident"], Value::Null);
    assert_eq!(home.status()["protocol"], 1);

    let mut foreground = home.command(&["executor"]).spawn().unwrap();
    let status = home
        .wait_for("the resident", |status| !status["resident"].is_null())
        .await;
    let resident = &status["resident"];
    assert_eq!(resident["idle"], true);
    assert_eq!(resident["pid"], foreground.id());
    assert_eq!(resident["version"], status["version"]);
    let (code, stdout, _) = home.run(&["executor", "status"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("zcode executor is running"), "{stdout}");
    // 只有一个：第二个报错退出。
    let (code, _, stderr) = home.run(&["executor"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("already running"), "{stderr}");
    // 目录与 socket 只给本用户。
    use std::os::unix::fs::PermissionsExt;
    let mode = |path: PathBuf| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(home.exec_dir()), 0o700);
    assert_eq!(mode(home.exec_dir().join("executor.sock")), 0o600);

    let (code, stdout, _) = home.run(&["executor", "stop"]);
    assert_eq!(code, 0, "{stdout}");
    assert!(stdout.contains("stopped"), "{stdout}");
    assert!(foreground.wait().unwrap().success());
    assert!(!home.exec_dir().join("executor.sock").exists());
    let (code, _, _) = home.run(&["executor", "stop"]);
    assert_eq!(code, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn proxies_start_one_resident_and_share_it() {
    let home = Home::new();
    let proxies = Proxies::new(&home);
    // 两个客户端同时连：两个代理，只有一个常驻进程。
    let (first, second) = tokio::join!(client(&proxies, "c1"), client(&proxies, "c2"));
    assert_eq!(
        first.welcome().unwrap().server_instance,
        second.welcome().unwrap().server_instance
    );
    let status = home
        .wait_for("two clients", |status| status["resident"]["clients"] == 2)
        .await;
    assert_eq!(status["resident"]["idle"], false);
    assert!(first.welcome().unwrap().ledger.is_some());
    // 客户端都走了：常驻进程还在（一直跑到 stop），空闲。
    first.close().await;
    second.close().await;
    home.wait_for("idle", |status| status["resident"]["idle"] == true)
        .await;
    let (code, _, _) = home.run(&["executor", "stop", "--if-idle"]);
    assert_eq!(code, 0);
    assert!(home.status()["resident"].is_null());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_command_keeps_running_when_the_proxy_is_killed() {
    let home = Home::new();
    let proxies = Proxies::new(&home);
    let client = client(&proxies, "c1").await;
    let instance = client.welcome().unwrap().server_instance;
    let machine = Arc::new(RemoteEnvironment::new(client.clone())).machine();
    let ws = home.workspace();
    let text = "for i in 1 2 3 4; do echo line$i; sleep 0.3; done";
    machine
        .commands()
        .start_command(&ws, context("op_loop"), command(&home.work, "t1", text))
        .await
        .unwrap();
    let mut events = machine
        .commands()
        .watch_command(OperationId::new("op_loop"), 0);
    let mut output = String::new();
    let mut killed = false;
    let mut exit = None;
    while let Some(event) = events.next().await {
        match event.unwrap() {
            CommandEvent::Output { data, .. } => {
                output.push_str(&String::from_utf8_lossy(&data));
                if !killed && output.contains("line1") {
                    // 断开通道：代理死了，常驻进程与命令照常。
                    kill(proxies.last());
                    killed = true;
                }
            }
            CommandEvent::Finished { exit_code, .. } => {
                exit = exit_code;
                break;
            }
            _ => {}
        }
    }
    assert_eq!(exit, Some(0));
    assert_eq!(output, "line1\nline2\nline3\nline4\n");
    assert!(
        proxies.count() >= 2,
        "the client reconnected through a new proxy"
    );
    assert_eq!(client.welcome().unwrap().server_instance, instance);
    client.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restarted_resident_marks_the_unfinished_mutation_interrupted() {
    let home = Home::new();
    let fifo = home.work.join("pipe");
    let made = std::process::Command::new("/usr/bin/mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(made.success());
    let proxies = Proxies::new(&home);
    let client = client(&proxies, "c1").await;
    let before = client.welcome().unwrap();
    let machine = Arc::new(RemoteEnvironment::new(client.clone())).machine();
    let ws = home.workspace();
    // 写一个没有读者的 FIFO：执行器打开它时一直等，调用停在“已登记、没有终态”。
    let write = WriteFile {
        path: EnvPath::new(fifo.to_string_lossy()),
        bytes: b"never".to_vec(),
        precondition: WritePrecondition::Any,
    };
    let pending = {
        let machine = machine.clone();
        let ws = ws.clone();
        tokio::spawn(async move {
            machine
                .files()
                .write_file(&ws, context("op_fifo"), write)
                .await
        })
    };
    let status = home
        .wait_for("the stuck write", |status| status["resident"]["calls"] == 1)
        .await;
    kill(u32::try_from(status["resident"]["pid"].as_u64().unwrap()).unwrap());
    // 客户端经新的代理重连，代理拉起新的常驻进程；它按日志把那次写记成中断，不重跑（重跑会再卡在 FIFO 上）。
    let result = tokio::time::timeout(Duration::from_secs(20), pending)
        .await
        .unwrap()
        .unwrap();
    let Err(EnvError::Io { message }) = result else {
        panic!("expected an interrupted write: {result:?}");
    };
    assert!(
        message.contains("restarted") || message.contains("interrupted"),
        "{message}"
    );
    let after = client.welcome().unwrap();
    assert_ne!(after.server_instance, before.server_instance);
    assert_eq!(
        after.ledger, before.ledger,
        "the ledger survives the restart"
    );
    let journal = std::fs::read_to_string(home.exec_dir().join("ledger/journal")).unwrap();
    assert!(journal.contains("\"t\":\"n\""), "{journal}");
    client.close().await;
}

/// `executor status --json` 的一行（remote-exec.md §19.6）就是部署时探测读的格式（ssh-connect，§19.9）：两边互相核对。
#[tokio::test(flavor = "multi_thread")]
async fn the_status_report_is_what_the_probe_reads() {
    let home = Home::new();
    let parse = |line: &str| {
        let output = format!("os Linux\narch x86_64\nhome /h\ncandidate {BIN}\t{line}\n");
        rust_cli_ssh_connect::Probe::parse(&output).unwrap()
    };
    let (_, idle, _) = home.run(&["executor", "status", "--json"]);
    let probe = parse(idle.trim());
    let report = probe.candidates[0].report.clone().unwrap();
    assert_eq!(report.protocol, 1);
    assert_eq!(report.executable.as_deref(), Some(BIN));
    assert!(probe.resident().is_none());
    let mut foreground = home.command(&["executor"]).spawn().unwrap();
    home.wait_for("the resident", |status| !status["resident"].is_null())
        .await;
    let (_, running, _) = home.run(&["executor", "status", "--json"]);
    let probe = parse(running.trim());
    let resident = probe.resident().unwrap();
    assert_eq!(resident.version, report.version);
    assert!(resident.idle);
    assert_eq!(resident.executable.as_deref(), Some(BIN));
    home.run(&["executor", "stop"]);
    assert!(foreground.wait().unwrap().success());
}
