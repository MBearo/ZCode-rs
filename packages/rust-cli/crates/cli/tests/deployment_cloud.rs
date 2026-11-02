//! 全云端的端到端（docs/specs/rust-cli/deployment.md §19.2，M4）：controller 与沙盒各在一个容器里（§5 的 ④，库用
//! SQLite），中间走 SSH。controller 容器的根文件系统只读，唯一可写的是卷 `/data`（HOME 与会话库都在里面，§9、§25）。
//!
//! 1. 第一次运行（新的 controller 容器）：工具、环境段、git 状态、规则、记忆、hook 都在沙盒；会话库在卷里；
//! 2. 第二次运行（另一个 controller 容器，同一个卷）：`-r` 续上，第一个请求带着第一次的历史——会话的事实只在卷里的库，
//!    不在 controller 进程里。
//!
//! 产物与跳过的规则同 `ssh_docker.rs`（`ZCODE_TEST_SSH_ARTIFACT` 是 Docker 架构的 musl 二进制，这里同时是 controller 的
//! 二进制与上传给沙盒的执行器）。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    clippy::print_stderr,
    clippy::unwrap_used,
    reason = "集成测试：起两个 Docker 容器、写配置与密钥、拉起 controller 容器"
)]

mod sse;
mod sshd;

use std::path::Path;
use std::process::Output;

use rust_cli_base::models::GLM_5;
use serde_json::{Value, json};
use sshd::{Image, Sshd, VERSION, artifact_or_skip, docker};

/// 沙盒里的项目：git 仓库（分支 trunk）、项目规则、一个文件。
const PROJECT: &str = "set -e; mkdir -p /srv/proj && cd /srv/proj \
    && git init -q -b trunk && git config user.name sandbox-user && git config user.email dev@example.com \
    && printf 'Rule from the sandbox.\\n' > AGENTS.md \
    && printf 'from the sandbox\\n' > notes.txt \
    && git add -A && git commit -q -m 'sandbox commit'";

/// controller 容器的镜像：只有 ssh 客户端（连沙盒）与 shell。
const CONTROLLER_DOCKERFILE: &str =
    "FROM alpine:3.20\nRUN apk add --no-cache openssh-client bash\n";

/// controller 的 ssh 配置里沙盒的名字。
const SANDBOX: &str = "zcode-cloud-sandbox";

/// ssh 要求私钥只有自己能读：只读挂进来的配置与密钥先复制进 tmpfs，再起 controller。
const LAUNCH: &str = "mkdir -p /tmp/ssh && cp /config/ssh_config /config/id_ed25519 /config/known_hosts /tmp/ssh/ \
    && chmod 600 /tmp/ssh/* && exec /opt/zcode/zcode-rs \"$@\"";

const UUID: &str = "6f1d4c2a-8b3e-4f5a-9c7d-2e1b0a9f8c7d";

/// controller 容器怎样到宿主机（模型夹具与沙盒发布的 ssh 端口都只听 127.0.0.1）：Docker Desktop 经
/// `host.docker.internal`；原生 Linux 的 Docker 用宿主机的网络。
fn host_access() -> (&'static [&'static str], &'static str) {
    if cfg!(target_os = "linux") {
        (&["--network", "host"], "127.0.0.1")
    } else {
        (
            &["--add-host", "host.docker.internal:host-gateway"],
            "host.docker.internal",
        )
    }
}

/// controller 的镜像与它的卷。丢掉时（测试失败也是）先用这个镜像删掉卷里 root 写的文件，再删镜像。
struct Controller {
    image: String,
    data: tempfile::TempDir,
}

impl Controller {
    /// 镜像的基础与沙盒相同，已经在本机。
    fn build(name: String) -> Self {
        let context = tempfile::tempdir().unwrap();
        std::fs::write(context.path().join("Dockerfile"), CONTROLLER_DOCKERFILE).unwrap();
        let (ok, _, err) = docker(&[
            "build",
            "-q",
            "-t",
            &name,
            &context.path().to_string_lossy(),
        ]);
        assert!(ok, "docker build: {err}");
        Self {
            image: name,
            data: tempfile::tempdir().unwrap(),
        }
    }
}

impl Drop for Controller {
    fn drop(&mut self) {
        let volume = format!("{}:/data", self.data.path().display());
        let _ = docker(&[
            "run",
            "--rm",
            "-v",
            &volume,
            &self.image,
            "rm",
            "-rf",
            "/data/home",
        ]);
        let _ = docker(&["rmi", "-f", &self.image]);
    }
}

/// 最后一条 user 消息（工具结果在里面）。合入 GLM 按 opus 处理之后，工具轮末尾另有一条 `<total_tokens>` 的 system
/// 提醒，所以不能取最后一条消息。
fn last_message(request: &Value) -> String {
    request["messages"]
        .as_array()
        .and_then(|messages| {
            messages
                .iter()
                .rev()
                .find(|message| message["role"] == "user")
        })
        .map(Value::to_string)
        .unwrap_or_default()
}

/// controller 读的输入：部署配置、ssh 配置、密钥与沙盒的主机公钥（只读挂到 /config）。
fn write_config(dir: &Path, sshd: &Sshd, host: &str) {
    let deployment = json!({"primary_env": {"ssh": {
        "destination": SANDBOX,
        "workdir": "/srv/proj",
        "ssh_config": "/tmp/ssh/ssh_config",
        "artifact": "/opt/zcode/zcode-rs",
    }}});
    std::fs::write(dir.join("deployment.json"), deployment.to_string()).unwrap();
    std::fs::write(
        dir.join("ssh_config"),
        format!(
            "Host {SANDBOX}\n  HostName {host}\n  Port {}\n  User root\n  IdentityFile /tmp/ssh/id_ed25519\n  \
             IdentitiesOnly yes\n  IdentityAgent none\n  UserKnownHostsFile /tmp/ssh/known_hosts\n  \
             GlobalKnownHostsFile /dev/null\n  StrictHostKeyChecking yes\n  LogLevel ERROR\n",
            sshd.port
        ),
    )
    .unwrap();
    std::fs::copy(sshd.path("id_ed25519"), dir.join("id_ed25519")).unwrap();
    let host_key = std::fs::read_to_string(sshd.path("ssh_host_ed25519_key.pub")).unwrap();
    let mut fields = host_key.split_whitespace();
    let (kind, key) = (fields.next().unwrap(), fields.next().unwrap());
    std::fs::write(
        dir.join("known_hosts"),
        format!("[{host}]:{} {kind} {key}\n", sshd.port),
    )
    .unwrap();
}

/// 一个新的 controller 容器跑一次 `zcode-rs`：根文件系统只读，/tmp 是 tmpfs，唯一的卷是 /data。容器里是 root：root 不能用
/// `bypassPermissions`（cli-args.md §5.1），所以调用方用 `--allowedTools` 预先放行这次要用的工具。
fn run_controller(
    (image, artifact, config, data): (&str, &Path, &Path, &Path),
    model: &str,
    args: &[&str],
) -> Output {
    let (access, _) = host_access();
    let mounts = [
        format!("{}:/opt/zcode/zcode-rs:ro", artifact.display()),
        format!("{}:/config:ro", config.display()),
        format!("{}:/data", data.display()),
    ];
    let mut command = std::process::Command::new("docker");
    command.args([
        "run",
        "--rm",
        "--read-only",
        "--tmpfs",
        "/tmp",
        "-w",
        "/tmp",
    ]);
    command.args(access);
    for mount in &mounts {
        command.args(["-v", mount]);
    }
    for (name, value) in [
        ("HOME", "/data/home"),
        ("ZCODE_DATA_BASE_DIR", "/data/home"),
        ("ZCODE_DEPLOYMENT", "/config/deployment.json"),
        ("ZCODE_MODEL_BASE_URL", model),
        ("ZCODE_API_KEY", "test-key"),
        ("ZCODE_MODEL", GLM_5),
        ("ZCODE_RUNTIME_ENV", "test"),
        ("SHELL", "/bin/bash"),
        ("PATH", "/usr/bin:/bin"),
    ] {
        command.args(["-e", &format!("{name}={value}")]);
    }
    command
        .args([image, "sh", "-c", LAUNCH, "zcode-rs"])
        .args(args);
    command.output().unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn both_planes_run_in_containers_and_the_session_lives_in_the_volume() {
    let Some(artifact) = artifact_or_skip() else {
        return;
    };
    let sshd = Sshd::start_with(Image::AlpineGit, "zcode-rs-test-cloud");
    sshd.wait_ready().await;
    let (ok, _, err) = sshd.ssh(PROJECT);
    assert!(ok, "{err}");

    let controller = Controller::build(format!("{}-controller", sshd.name));

    let (_, host) = host_access();
    let config = tempfile::tempdir().unwrap();
    write_config(config.path(), &sshd, host);
    // 卷：用户级配置（Node 的形状，config.md §2）在 controller 的 HOME 里；hook 把自己跑在哪写进沙盒的 /tmp。
    let data = controller.data.path();
    let user_config = data.join("home/.zcode/cli/config.json");
    std::fs::create_dir_all(user_config.parent().unwrap()).unwrap();
    let hooks = json!({"hooks": {"enabled": true, "events": {"UserPromptSubmit": [{"hooks": [
        {"type": "command", "command": "echo \"hook in $(uname -s) at $(pwd)\" > /tmp/hook-ran"}
    ]}]}}});
    std::fs::write(&user_config, hooks.to_string()).unwrap();

    let bash = json!({
        "command": "uname -s; pwd; test -f /etc/alpine-release && echo inside-alpine",
        "description": "Where am I"
    })
    .to_string();
    let read = json!({"file_path": "notes.txt"}).to_string();
    let fixture = sse::serve(vec![
        sse::tools_reply(&[sse::tool_block(0, "toolu_1", "Bash", &bash)]),
        sse::tools_reply(&[sse::tool_block(0, "toolu_2", "Read", &read)]),
        sse::text_reply("done"),
        sse::text_reply("again"),
    ]);
    let model = fixture.url.replace("127.0.0.1", host);
    let inputs = (
        controller.image.as_str(),
        artifact.as_path(),
        config.path(),
        data,
    );

    // 第一次运行。
    let first_run = run_controller(
        inputs,
        &model,
        &[
            "-p",
            "check the sandbox",
            "--allowedTools",
            "Bash,Read",
            "--session-id",
            UUID,
        ],
    );
    assert!(
        first_run.status.success(),
        "{}",
        String::from_utf8_lossy(&first_run.stderr)
    );
    let requests = fixture.main_requests();
    assert_eq!(requests.len(), 3);
    let first = requests[0].to_string();
    assert!(first.contains("Platform: linux"), "{first}");
    assert!(first.contains("/srv/proj"), "{first}");
    assert!(first.contains("Current branch: trunk"), "{first}");
    assert!(first.contains("sandbox commit"), "{first}");
    assert!(first.contains("Rule from the sandbox."), "{first}");
    assert!(
        first.contains("/root/.zcode/projects/-srv-proj/memory/"),
        "{first}"
    );
    let ran = last_message(&requests[1]);
    assert!(ran.contains("Linux"), "{ran}");
    assert!(ran.contains("inside-alpine"), "{ran}");
    let read = last_message(&requests[2]);
    assert!(read.contains("from the sandbox"), "{read}");
    let (ok, out, err) = sshd.ssh("cat /tmp/hook-ran");
    assert!(ok, "the hook ran in the sandbox: {err}");
    assert_eq!(out.trim(), "hook in Linux at /srv/proj");
    let (ok, _, err) = sshd.ssh("test -d /root/.zcode/projects/-srv-proj/memory");
    assert!(ok, "the memory directory is in the sandbox: {err}");
    // controller 这边：会话库在卷里，记忆不在。
    let home = data.join("home/.zcode");
    assert!(
        home.join("cli/db/db.sqlite").is_file(),
        "the session database is in the volume"
    );
    assert!(
        !home.join("projects").exists(),
        "no memory directory on the controller"
    );

    // 第二次运行：另一个 controller 容器，同一个卷，续上同一个会话。
    let second_run = run_controller(
        inputs,
        &model,
        &[
            "-p",
            "continue in the cloud",
            "--allowedTools",
            "Bash,Read",
            "-r",
            UUID,
        ],
    );
    assert!(
        second_run.status.success(),
        "{}",
        String::from_utf8_lossy(&second_run.stderr)
    );
    let requests = fixture.main_requests();
    assert_eq!(requests.len(), 4);
    let resumed = requests[3].to_string();
    assert!(resumed.contains("check the sandbox"), "{resumed}");
    assert!(resumed.contains("inside-alpine"), "{resumed}");
    assert!(resumed.contains("continue in the cloud"), "{resumed}");

    // 收尾：停掉沙盒里的常驻执行器（镜像与卷由 `Controller` 丢掉时收拾）。
    let installed = format!("/root/.zcode/rust-cli/exec/bin/{VERSION}/zcode-rs");
    let (ok, out, err) = sshd.ssh(&format!("{installed} executor stop"));
    assert!(ok, "{out}{err}");
}
