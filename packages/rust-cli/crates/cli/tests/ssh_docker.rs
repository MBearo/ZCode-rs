//! SSH 的真实进程测试（docs/specs/rust-cli/remote-exec.md §19.12）：本机有 Docker 时起一个 Linux sshd 容器（测试专用的
//! 临时密钥、`-F` 指定的临时 ssh 配置、`UserKnownHostsFile` 指临时文件，不碰用户的 `~/.ssh`），经真实的 `ssh`：
//!
//! 1. 部署：探测、上传本机的 Linux 产物、远端校验 sha256；第二次部署不再上传；
//! 2. 连接状态机连上，执行端口的一部分（文件、搜索、命令）在容器里执行；
//! 3. 杀掉本机的 ssh 进程：客户端重连，正在跑的命令继续，输出接着取到；
//! 4. `-p` 经只给测试的 SSH 入口（`_ZCODE_ASSUME_SSH_EXECUTOR`），Bash 在容器里执行。
//!
//! Linux 产物由 `test:rust-cli`（scripts/rust-cli/test.mjs）交叉编译后经 `ZCODE_TEST_SSH_ARTIFACT` 交进来；没有 Docker 或没有
//! 产物时跳过并写明原因（`ZCODE_TEST_SSH_SKIP`）。容器与镜像用完删掉。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    clippy::print_stderr,
    clippy::unwrap_used,
    reason = "集成测试：生成临时密钥、起 Docker 容器、拉起 ssh 与 zcode 进程；跳过时写明原因"
)]

mod sse;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use rust_cli_env_remote::RemoteEnvironment;
use rust_cli_exec_client::{ClientOptions, Reconnect};
use rust_cli_exec_transport::{Connector, LinkError, Provisioned, Provisioner};
use rust_cli_exec_wire::handshake::PROTOCOL;
use rust_cli_kernel_api::env::{
    CommandEvent, EnvPath, OperationContext, Origin, ReadFile, StartCommand, WorkspaceRef,
    WritePrecondition,
};
use rust_cli_kernel_api::env_files::{GrepMode, GrepOutcome, GrepSearch, PutFile};
use rust_cli_kernel_api::env_shell::CwdScope;
use rust_cli_kernel_api::ids::{
    EnvironmentId, Generation, OperationId, OwnerEpoch, SessionId, ShellTaskId,
};
use rust_cli_machines::{Endpoint, Machine, MachineState};
use rust_cli_ssh_connect::{SshConnector, SshProvisioner, SshTarget, Wanted};
use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_zcode-rs");
const VERSION: &str = env!("CARGO_PKG_VERSION");
const BASE_IMAGE: &str = "alpine:3.20";
const HOST: &str = "zcode-test";

fn run(program: &str, args: &[&str]) -> (bool, String, String) {
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn docker(args: &[&str]) -> (bool, String, String) {
    run("docker", args)
}

/// 容器与它的镜像；丢掉时删掉（镜像的基础层是这次拉的才删）。
struct Sshd {
    name: String,
    pulled_base: bool,
    port: u16,
    dir: tempfile::TempDir,
}

impl Drop for Sshd {
    fn drop(&mut self) {
        let _ = docker(&["rm", "-f", &self.name]);
        let _ = docker(&["rmi", "-f", &self.name]);
        if self.pulled_base {
            let _ = docker(&["rmi", BASE_IMAGE]);
        }
    }
}

const DOCKERFILE: &str = r#"FROM alpine:3.20
RUN apk add --no-cache openssh-server bash \
 && sed -i 's#^root:x:0:0:root:/root:/bin/ash$#root:x:0:0:root:/root:/bin/bash#' /etc/passwd \
 && sed -i 's#^root:[^:]*:#root:*:#' /etc/shadow \
 && mkdir -p /root/.ssh && chmod 700 /root/.ssh
COPY sshd_config /etc/ssh/sshd_config
COPY ssh_host_ed25519_key /etc/ssh/ssh_host_ed25519_key
COPY authorized_keys /root/.ssh/authorized_keys
RUN chown root:root /etc/ssh/ssh_host_ed25519_key /root/.ssh/authorized_keys \
 && chmod 600 /etc/ssh/ssh_host_ed25519_key /root/.ssh/authorized_keys
EXPOSE 22
CMD ["/usr/sbin/sshd", "-D", "-e"]
"#;

const SSHD_CONFIG: &str = "Port 22
HostKey /etc/ssh/ssh_host_ed25519_key
PermitRootLogin prohibit-password
PubkeyAuthentication yes
PasswordAuthentication no
KbdInteractiveAuthentication no
AuthorizedKeysFile .ssh/authorized_keys
UseDNS no
";

impl Sshd {
    fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = |name: &str| dir.path().join(name);
        for key in ["id_ed25519", "ssh_host_ed25519_key"] {
            let output = Command::new("ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", "", "-C", "zcode-test", "-f"])
                .arg(path(key))
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "ssh-keygen: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let context = path("context");
        std::fs::create_dir_all(&context).unwrap();
        std::fs::write(context.join("Dockerfile"), DOCKERFILE).unwrap();
        std::fs::write(context.join("sshd_config"), SSHD_CONFIG).unwrap();
        std::fs::copy(
            path("ssh_host_ed25519_key"),
            context.join("ssh_host_ed25519_key"),
        )
        .unwrap();
        std::fs::copy(path("id_ed25519.pub"), context.join("authorized_keys")).unwrap();
        let pulled_base = !docker(&["image", "inspect", BASE_IMAGE]).0;
        let name = format!(
            "zcode-rs-test-sshd-{}-{:x}",
            std::process::id(),
            rust_cli_base::random::u64() & 0xffff_ffff
        );
        let mut sshd = Self {
            name,
            pulled_base,
            port: 0,
            dir,
        };
        let context = context.to_string_lossy().into_owned();
        let (ok, _, err) = docker(&["build", "-q", "-t", &sshd.name, &context]);
        assert!(ok, "docker build: {err}");
        let (ok, _, err) = docker(&[
            "run",
            "-d",
            "--name",
            &sshd.name,
            "-p",
            "127.0.0.1::22",
            &sshd.name,
        ]);
        assert!(ok, "docker run: {err}");
        let (ok, out, err) = docker(&["port", &sshd.name, "22/tcp"]);
        assert!(ok, "docker port: {err}");
        sshd.port = out
            .lines()
            .find_map(|line| line.strip_prefix("127.0.0.1:"))
            .and_then(|port| port.trim().parse().ok())
            .unwrap();
        let host_key = std::fs::read_to_string(sshd.path("ssh_host_ed25519_key.pub")).unwrap();
        let mut fields = host_key.split_whitespace();
        let (kind, key) = (fields.next().unwrap(), fields.next().unwrap());
        std::fs::write(
            sshd.path("known_hosts"),
            format!("[127.0.0.1]:{} {kind} {key}\n", sshd.port),
        )
        .unwrap();
        std::fs::create_dir_all(sshd.path("home")).unwrap();
        std::fs::write(
            sshd.path("ssh_config"),
            format!(
                "Host {HOST}\n  HostName 127.0.0.1\n  Port {}\n  User root\n  IdentityFile {}\n  IdentitiesOnly yes\n  \
                 IdentityAgent none\n  UserKnownHostsFile {}\n  GlobalKnownHostsFile /dev/null\n  StrictHostKeyChecking yes\n  \
                 LogLevel ERROR\n",
                sshd.port,
                sshd.path("id_ed25519").display(),
                sshd.path("known_hosts").display()
            ),
        )
        .unwrap();
        sshd
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn config(&self) -> String {
        self.path("ssh_config").to_string_lossy().into_owned()
    }

    /// ssh 进程的环境：临时 HOME，没有 ssh-agent。
    fn env(&self) -> Vec<(String, String)> {
        vec![
            (
                "HOME".to_owned(),
                self.path("home").to_string_lossy().into_owned(),
            ),
            ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
        ]
    }

    fn target(&self) -> SshTarget {
        SshTarget {
            destination: HOST.to_owned(),
            program: PathBuf::from("ssh"),
            config: Some(self.path("ssh_config")),
            options: Vec::new(),
            env: self.env(),
        }
    }

    /// 经 ssh 在容器里跑一条命令。
    fn ssh(&self, command: &str) -> (bool, String, String) {
        let output = Command::new("ssh")
            .args([
                "-F",
                &self.config(),
                "-T",
                "-o",
                "BatchMode=yes",
                HOST,
                command,
            ])
            .env_clear()
            .envs(self.env())
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    async fn wait_ready(&self) {
        let mut last = String::new();
        for _ in 0..120 {
            let (ok, _, err) = self.ssh("true");
            if ok {
                return;
            }
            last = err;
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        panic!("sshd did not come up: {last}");
    }

    /// 结束本机经这个配置跑 `executor --stdio` 的 ssh 进程（模拟断线）。
    fn kill_local_ssh(&self) -> usize {
        let (_, out, _) = run("ps", &["-axo", "pid=,command="]);
        let config = self.config();
        let pids: Vec<String> = out
            .lines()
            .filter(|line| line.contains(&config) && line.contains("--stdio"))
            .filter_map(|line| line.split_whitespace().next().map(str::to_owned))
            .collect();
        for pid in &pids {
            let _ = run("/bin/kill", &["-9", pid]);
        }
        pids.len()
    }
}

struct TestEndpoint {
    target: SshTarget,
    provisioner: SshProvisioner,
}

impl Endpoint for TestEndpoint {
    fn describe(&self) -> String {
        self.target.describe()
    }

    fn provisioner(&self) -> &dyn Provisioner {
        &self.provisioner
    }

    fn connector(&self, provisioned: &Provisioned) -> Result<Arc<dyn Connector>, LinkError> {
        Ok(Arc::new(SshConnector::new(
            self.target.clone(),
            provisioned,
        )?))
    }
}

fn wanted() -> Wanted {
    Wanted {
        version: VERSION.to_owned(),
        protocol: PROTOCOL,
    }
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

/// 没有 Docker 或没有 Linux 产物时跳过（写明原因）；有时返回产物的路径。
fn artifact_or_skip() -> Option<PathBuf> {
    let reason = std::env::var("ZCODE_TEST_SSH_SKIP").ok();
    match std::env::var("ZCODE_TEST_SSH_ARTIFACT") {
        Ok(path) if Path::new(&path).is_file() && docker(&["info"]).0 => Some(PathBuf::from(path)),
        _ => {
            eprintln!(
                "SSH Docker test skipped: {}",
                reason.unwrap_or_else(|| {
                    "ZCODE_TEST_SSH_ARTIFACT is not set (run pnpm test:rust-cli)".to_owned()
                })
            );
            None
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_ssh_machine_is_provisioned_used_and_survives_a_dropped_link() {
    let Some(artifact) = artifact_or_skip() else {
        return;
    };
    let sshd = Sshd::start();
    sshd.wait_ready().await;
    let installed = format!("/root/.zcode/rust-cli/exec/bin/{VERSION}/zcode-rs");

    // 1. 部署：上传、远端 sha256 与本机相同、权限只给本用户；第二次不再上传。
    let provisioner = SshProvisioner::new(sshd.target(), wanted(), vec![artifact.clone()]);
    let provisioned = provisioner.provision().await.unwrap();
    assert_eq!(provisioned.program, installed);
    let (_, remote_sum, _) = sshd.ssh(&format!(
        "sha256sum {installed}; stat -c '%a %Y' {installed}"
    ));
    let (_, local_sum, _) = run("shasum", &["-a", "256", &artifact.to_string_lossy()]);
    let mut lines = remote_sum.lines();
    assert_eq!(
        lines.next().unwrap().split_whitespace().next(),
        local_sum.split_whitespace().next()
    );
    let first_stat = lines.next().unwrap().to_owned();
    assert!(first_stat.starts_with("700 "), "{first_stat}");
    let again = provisioner.provision().await.unwrap();
    assert_eq!(again.program, installed);
    let (_, second_stat, _) = sshd.ssh(&format!("stat -c '%a %Y' {installed}"));
    assert_eq!(second_stat.trim(), first_stat, "not uploaded again");

    // 2. 状态机连上；执行端口的一部分在容器里执行。
    let endpoint = Arc::new(TestEndpoint {
        target: sshd.target(),
        provisioner: SshProvisioner::new(sshd.target(), wanted(), vec![artifact.clone()]),
    });
    let mut options = ClientOptions::new(VERSION, "ssh-test");
    options.reconnect = Reconnect {
        attempts: 20,
        delay: Duration::from_millis(100),
        max_delay: Duration::from_millis(500),
    };
    let machine = Machine::new(endpoint, options);
    let client = machine.connect().await.unwrap();
    let welcome = client.welcome().unwrap();
    assert_eq!(welcome.os, "linux");
    assert_eq!(welcome.home.as_deref(), Some("/root"));
    let MachineState::Ready(ready) = machine.state() else {
        panic!("not ready: {:?}", machine.state());
    };
    let remote = Arc::new(RemoteEnvironment::new(client.clone())).machine();
    let root = "/tmp/zc";
    let ws = WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(root),
    };
    remote
        .files()
        .put_file(
            &ws,
            context("op_put"),
            PutFile {
                path: EnvPath::new(format!("{root}/notes/a.txt")),
                bytes: b"hello from the executor\n".to_vec(),
                precondition: WritePrecondition::Absent,
                create_parents: true,
            },
        )
        .await
        .unwrap();
    let read = remote
        .files()
        .read_file(
            &ws,
            ReadFile {
                path: EnvPath::new(format!("{root}/notes/a.txt")),
            },
        )
        .await
        .unwrap();
    assert_eq!(read.bytes, b"hello from the executor\n");
    // 搜索走执行器里内嵌的 rg（Linux 的静态产物）。
    let search = GrepSearch {
        root: EnvPath::new(root),
        pattern: "executor".to_owned(),
        case_insensitive: false,
        multiline: false,
        globs: Vec::new(),
        file_type: None,
        hidden: false,
        mode: GrepMode::FilesWithMatches,
    };
    let found = remote.search().grep(&ws, search).await.unwrap();
    let GrepOutcome::Files { files } = &found else {
        panic!("{found:?}");
    };
    assert!(
        files
            .iter()
            .any(|hit| hit.path.as_str().ends_with("notes/a.txt")),
        "{found:?}"
    );

    // 3. 命令跑着时杀掉本机的 ssh：客户端经新的 ssh 重连，命令继续，输出接着取到；执行器还是同一个实例。
    // 状态的变化由一个任务记下（watch 只留最新值：命令结束时再看就只剩 Ready）。
    let recorded = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorder = {
        let mut states = machine.states();
        let recorded = recorded.clone();
        rust_cli_base::owned(async move {
            while states.changed().await.is_ok() {
                let state = states.borrow_and_update().clone();
                recorded.lock().unwrap().push(state);
            }
        })
    };
    let text = "for i in 1 2 3 4; do echo line$i; sleep 0.4; done; uname -s";
    remote
        .commands()
        .start_command(
            &ws,
            context("op_loop"),
            StartCommand {
                command: text.to_owned(),
                cwd: EnvPath::new(root),
                timeout_ms: Some(60_000),
                task: ShellTaskId::new("t1"),
                shell: Default::default(),
                cwd_scope: CwdScope::Pinned,
                background: None,
                sandbox: None,
                hook: None,
                on_timeout: None,
                socket: None,
            },
        )
        .await
        .unwrap();
    let mut events = remote
        .commands()
        .watch_command(OperationId::new("op_loop"), 0);
    let mut output = String::new();
    let mut killed = 0;
    let mut exit = None;
    while let Some(event) = tokio::time::timeout(Duration::from_secs(60), events.next())
        .await
        .unwrap()
    {
        match event.unwrap() {
            CommandEvent::Output { data, .. } => {
                output.push_str(&String::from_utf8_lossy(&data));
                if killed == 0 && output.contains("line1") {
                    killed = sshd.kill_local_ssh();
                }
            }
            CommandEvent::Finished { exit_code, .. } => {
                exit = exit_code;
                break;
            }
            _ => {}
        }
    }
    assert!(killed >= 1, "an ssh process was killed");
    assert_eq!(exit, Some(0));
    assert_eq!(output, "line1\nline2\nline3\nline4\nLinux\n");
    let seen = recorded.lock().unwrap().clone();
    drop(recorder);
    assert!(
        seen.iter()
            .any(|state| matches!(state, MachineState::Reconnecting { .. })),
        "{seen:?}"
    );
    assert!(matches!(
        &*client.states().borrow(),
        rust_cli_exec_client::ConnectionState::Connected { epoch, .. } if *epoch >= 2
    ));
    let MachineState::Ready(after) = machine.state() else {
        panic!("not ready again: {seen:?}");
    };
    assert_eq!(after.server_instance, ready.server_instance);
    drop(remote);
    machine.disconnect().await;

    // 4. `-p` 经只给测试的 SSH 入口：Bash 在容器里执行（工作区的路径在容器里也建好：远端的工作目录在 R3）。
    let work = tempfile::tempdir().unwrap();
    let work_path = std::fs::canonicalize(work.path()).unwrap();
    let home = tempfile::tempdir().unwrap();
    let (ok, _, err) = sshd.ssh(&format!("mkdir -p '{}'", work_path.display()));
    assert!(ok, "{err}");
    let bash = serde_json::json!({
        "command": "uname -s; echo marker-$((6*7))",
        "description": "Print the system name"
    })
    .to_string();
    let fixture = sse::serve(vec![
        sse::tools_reply(&[sse::tool_block(0, "toolu_1", "Bash", &bash)]),
        sse::text_reply("done"),
    ]);
    let output = Command::new(BIN)
        .args(["-p", "run it", "--permission-mode", "bypassPermissions"])
        .current_dir(&work_path)
        .env_clear()
        .env("HOME", home.path())
        .env("PATH", "/usr/bin:/bin")
        .env("SHELL", "/bin/bash")
        .env("ZCODE_DATA_BASE_DIR", home.path())
        .env("ZCODE_MODEL_BASE_URL", &fixture.url)
        .env("ZCODE_API_KEY", "test-key")
        .env("ZCODE_MODEL", "glm-5")
        .env("_ZCODE_ASSUME_SSH_EXECUTOR", HOST)
        .env("_ZCODE_ASSUME_SSH_CONFIG", sshd.config())
        .env("_ZCODE_ASSUME_SSH_ARTIFACT", &artifact)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let requests = fixture.main_requests();
    let result = requests
        .get(1)
        .and_then(|request| request["messages"].as_array())
        .and_then(|messages| messages.last())
        .map(Value::to_string)
        .unwrap_or_default();
    assert!(result.contains("Linux"), "{result}");
    assert!(result.contains("marker-42"), "{result}");

    // 收尾：停掉容器里的常驻执行器（容器随后删掉）。
    let (ok, out, err) = sshd.ssh(&format!("{installed} executor stop"));
    assert!(ok, "{out}{err}");
}
