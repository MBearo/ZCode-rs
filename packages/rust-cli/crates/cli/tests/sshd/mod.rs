//! SSH 的 Docker 测试共用的夹具（docs/specs/rust-cli/remote-exec.md §19.12、§23.8）：Linux sshd 容器（测试专用的临时密钥、
//! `-F` 指定的临时 ssh 配置、`UserKnownHostsFile` 指临时文件，不碰用户的 `~/.ssh`）、测试用的端点与跳过的规则。
#![allow(dead_code, reason = "不同的测试文件用到的辅助函数不同")]
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use rust_cli_exec_transport::{Connector, LinkError, Provisioned, Provisioner};
use rust_cli_exec_wire::handshake::PROTOCOL;
use rust_cli_kernel_api::env::{OperationContext, Origin};
use rust_cli_kernel_api::ids::{EnvironmentId, Generation, OperationId, OwnerEpoch, SessionId};
use rust_cli_machines::Endpoint;
use rust_cli_ssh_connect::{SshConnector, SshProvisioner, SshTarget, Wanted};

/// F107（remote-exec.md §28.10）：本机就是 macOS 时的真实 SSH 夹具（不用 Docker）。
#[cfg(unix)]
pub mod local;

pub const BIN: &str = env!("CARGO_BIN_EXE_zcode-rs");
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const BASE_IMAGE: &str = "alpine:3.20";
pub const HOST: &str = "zcode-test";

/// 容器的系统（F100，remote-exec.md §26.9 的测试）：alpine 是 musl；debian 是 glibc。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Image {
    Alpine,
    Debian,
}

impl Image {
    fn base(self) -> &'static str {
        match self {
            Self::Alpine => BASE_IMAGE,
            Self::Debian => "debian:bookworm-slim",
        }
    }

    fn dockerfile(self) -> &'static str {
        match self {
            Self::Alpine => DOCKERFILE,
            Self::Debian => DEBIAN_DOCKERFILE,
        }
    }
}

pub fn run(program: &str, args: &[&str]) -> (bool, String, String) {
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

pub fn docker(args: &[&str]) -> (bool, String, String) {
    run("docker", args)
}

/// 容器与它的镜像；丢掉时删掉（镜像的基础层是这次拉的才删）。
pub struct Sshd {
    pub name: String,
    image: Image,
    pulled_base: bool,
    pub port: u16,
    dir: tempfile::TempDir,
}

impl Drop for Sshd {
    fn drop(&mut self) {
        let _ = docker(&["rm", "-f", &self.name]);
        let _ = docker(&["rmi", "-f", &self.name]);
        if self.pulled_base {
            let _ = docker(&["rmi", self.image.base()]);
        }
    }
}

pub const DOCKERFILE: &str = r#"FROM alpine:3.20
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

/// glibc 的容器（F100）：debian 的 sshd 要 `/run/sshd`；root 的口令本来就是锁住的。
pub const DEBIAN_DOCKERFILE: &str = r#"FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends openssh-server bash procps \
 && rm -rf /var/lib/apt/lists/* \
 && mkdir -p /run/sshd /root/.ssh && chmod 700 /root/.ssh
COPY sshd_config /etc/ssh/sshd_config
COPY ssh_host_ed25519_key /etc/ssh/ssh_host_ed25519_key
COPY authorized_keys /root/.ssh/authorized_keys
RUN chown root:root /etc/ssh/ssh_host_ed25519_key /root/.ssh/authorized_keys \
 && chmod 600 /etc/ssh/ssh_host_ed25519_key /root/.ssh/authorized_keys
EXPOSE 22
CMD ["/usr/sbin/sshd", "-D", "-e"]
"#;

pub const SSHD_CONFIG: &str = "Port 22
HostKey /etc/ssh/ssh_host_ed25519_key
PermitRootLogin prohibit-password
PubkeyAuthentication yes
PasswordAuthentication no
KbdInteractiveAuthentication no
AuthorizedKeysFile .ssh/authorized_keys
AllowAgentForwarding yes
UseDNS no
";

impl Sshd {
    pub fn start() -> Self {
        Self::start_with(Image::Alpine, "zcode-rs-test-sshd")
    }

    /// `prefix`：容器与镜像名字的前缀（每个测试线自己的，不和别的容器撞名）。
    pub fn start_with(image: Image, prefix: &str) -> Self {
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
        std::fs::write(context.join("Dockerfile"), image.dockerfile()).unwrap();
        std::fs::write(context.join("sshd_config"), SSHD_CONFIG).unwrap();
        std::fs::copy(
            path("ssh_host_ed25519_key"),
            context.join("ssh_host_ed25519_key"),
        )
        .unwrap();
        std::fs::copy(path("id_ed25519.pub"), context.join("authorized_keys")).unwrap();
        let pulled_base = !docker(&["image", "inspect", image.base()]).0;
        let name = format!(
            "{prefix}-{}-{:x}",
            std::process::id(),
            rust_cli_base::random::u64() & 0xffff_ffff
        );
        let mut sshd = Self {
            name,
            image,
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

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    pub fn config(&self) -> String {
        self.path("ssh_config").to_string_lossy().into_owned()
    }

    /// ssh 进程的环境：临时 HOME，没有 ssh-agent。
    pub fn env(&self) -> Vec<(String, String)> {
        vec![
            (
                "HOME".to_owned(),
                self.path("home").to_string_lossy().into_owned(),
            ),
            ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
        ]
    }

    pub fn target(&self) -> SshTarget {
        SshTarget {
            destination: HOST.to_owned(),
            program: PathBuf::from("ssh"),
            config: Some(self.path("ssh_config")),
            options: Vec::new(),
            env: self.env(),
        }
    }

    /// 经 ssh 在容器里跑一条命令。
    pub fn ssh(&self, command: &str) -> (bool, String, String) {
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

    pub async fn wait_ready(&self) {
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
    pub fn kill_local_ssh(&self) -> usize {
        self.kill_ssh_using(&self.config())
    }

    /// 同 [`Self::kill_local_ssh`]，按给的 ssh 配置文件找进程（F96：开了 agent 转发的那份配置）。
    pub fn kill_ssh_using(&self, config: &str) -> usize {
        let (_, out, _) = run("ps", &["-axo", "pid=,command="]);
        let config = config.to_owned();
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

/// 测试专用的 ssh-agent（远端执行 F96，remote-exec.md §23.2）：`ssh-agent -D -a <临时目录>/agent.sock` 在前台跑，装着
/// 测试的私钥；丢掉时结束。不碰用户的 agent。
pub struct Agent {
    child: std::process::Child,
    pub socket: PathBuf,
    _dir: tempfile::TempDir,
}

impl Agent {
    pub async fn start(sshd: &Sshd) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("agent.sock");
        let child = Command::new("ssh-agent")
            .arg("-D")
            .arg("-a")
            .arg(&socket)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let agent = Self {
            child,
            socket,
            _dir: dir,
        };
        for _ in 0..200 {
            if agent.socket.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let added = Command::new("ssh-add")
            .arg(sshd.path("id_ed25519"))
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("SSH_AUTH_SOCK", &agent.socket)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(
            added.status.success(),
            "ssh-add: {}",
            String::from_utf8_lossy(&added.stderr)
        );
        agent
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Sshd {
    /// 开了 agent 转发的 ssh 配置（F96）：同一个目标，`ForwardAgent yes`，agent 是测试专用的那个。
    pub fn forwarding_config(&self, agent: &Agent) -> PathBuf {
        let base = std::fs::read_to_string(self.path("ssh_config")).unwrap();
        let text = base.replace(
            "  IdentityAgent none\n",
            &format!(
                "  IdentityAgent {}\n  ForwardAgent yes\n",
                agent.socket.display()
            ),
        );
        assert_ne!(text, base);
        let path = self.path("ssh_config_forwarding");
        std::fs::write(&path, text).unwrap();
        path
    }
}

pub struct TestEndpoint {
    pub target: SshTarget,
    pub provisioner: SshProvisioner,
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

pub fn wanted() -> Wanted {
    Wanted {
        version: VERSION.to_owned(),
        protocol: PROTOCOL,
    }
}

pub fn context(operation: &str) -> OperationContext {
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
pub fn artifact_or_skip() -> Option<PathBuf> {
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
