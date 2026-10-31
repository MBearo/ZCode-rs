//! SSH 的真实进程测试（docs/specs/rust-cli/remote-exec.md §19.12）：本机有 Docker 时起一个 Linux sshd 容器（测试专用的
//! 临时密钥、`-F` 指定的临时 ssh 配置、`UserKnownHostsFile` 指临时文件，不碰用户的 `~/.ssh`），经真实的 `ssh`：
//!
//! 1. 部署：探测、上传本机的 Linux 产物、远端校验 sha256；第二次部署不再上传；
//! 2. 连接状态机连上，执行端口的一部分（文件、搜索、命令）在容器里执行；
//! 3. 杀掉本机的 ssh 进程：客户端重连，正在跑的命令继续，输出接着取到；
//! 4. `-p` 经只给测试的 SSH 入口（`_ZCODE_ASSUME_SSH_EXECUTOR`），Bash 在容器里执行；
//! 5. 远端 MCP（§22.10）：绑定到 dev-a 的 stdio 服务器（仓库里的 bash 夹具，复制进容器）由容器里的执行器起，工具的结果
//!    来自容器里的文件与命令，`${HOME}` 是容器里的。
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
mod sshd;

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use rust_cli_base::models::GLM_5;
use rust_cli_env_remote::RemoteEnvironment;
use rust_cli_exec_client::{ClientOptions, Reconnect};
use rust_cli_exec_transport::Provisioner;
use rust_cli_kernel_api::env::{
    CommandEvent, EnvPath, ReadFile, StartCommand, WorkspaceRef, WritePrecondition,
};
use rust_cli_kernel_api::env_files::{GrepMode, GrepOutcome, GrepSearch, PutFile};
use rust_cli_kernel_api::env_shell::CwdScope;
use rust_cli_kernel_api::ids::{EnvironmentId, ShellTaskId};
use rust_cli_machines::{Machine, MachineState};
use rust_cli_ssh_connect::SshProvisioner;
use serde_json::Value;
use sshd::{
    BIN, HOST, Sshd, TestEndpoint, VERSION, artifact_or_skip, context, docker, run, wanted,
};

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
        attempts: Some(20),
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
    let mut events = remote
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
                env: Default::default(),
            },
        )
        .await
        .unwrap();
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
        .env("ZCODE_MODEL", GLM_5)
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
        // 最后一条 user 消息（有卡片形状的模型把提醒放在其后的 system 消息里）。
        .and_then(|messages| {
            messages
                .iter()
                .rev()
                .find(|message| message["role"] == "user")
        })
        .map(Value::to_string)
        .unwrap_or_default();
    assert!(result.contains("Linux"), "{result}");
    assert!(result.contains("marker-42"), "{result}");

    // 收尾：停掉容器里的常驻执行器（容器随后删掉）。
    let (ok, out, err) = sshd.ssh(&format!("{installed} executor stop"));
    assert!(ok, "{out}{err}");
}

/// 远端执行 R3（remote-exec.md §20.11）：机器清单里的 dev-a 经 SSH 连上（R2 的状态机与 ssh-connect）；脚本模型调用
/// `Bash{machine}` 在容器里跑命令，结果回到本机；`Read{machine}` 用相对路径读容器里 dev-a 工作目录下的文件。
#[tokio::test(flavor = "multi_thread")]
async fn a_listed_machine_runs_bash_and_read_in_the_container() {
    let Some(artifact) = artifact_or_skip() else {
        return;
    };
    let sshd = Sshd::start();
    sshd.wait_ready().await;
    let (ok, _, err) =
        sshd.ssh("mkdir -p /srv/proj && printf 'from the container\\n' > /srv/proj/notes.txt");
    assert!(ok, "{err}");
    let work = tempfile::tempdir().unwrap();
    let work_path = std::fs::canonicalize(work.path()).unwrap();
    let home = tempfile::tempdir().unwrap();
    let list = home.path().join(".zcode/rust-cli/machines.json");
    std::fs::create_dir_all(list.parent().unwrap()).unwrap();
    std::fs::write(
        &list,
        serde_json::json!({"version": 1, "machines": [
            {"name": "dev-a", "ssh": HOST, "workingDirectory": "/srv/proj", "connected": true}
        ]})
        .to_string(),
    )
    .unwrap();
    let bash = serde_json::json!({
        "command": "uname -s; pwd; echo marker-$((6*7))",
        "description": "Where am I",
        "machine": "dev-a"
    })
    .to_string();
    let read = serde_json::json!({"file_path": "notes.txt", "machine": "dev-a"}).to_string();
    let fixture = sse::serve(vec![
        sse::tools_reply(&[sse::tool_block(0, "toolu_1", "Bash", &bash)]),
        sse::tools_reply(&[sse::tool_block(0, "toolu_2", "Read", &read)]),
        sse::text_reply("done"),
    ]);
    let output = Command::new(BIN)
        .args([
            "-p",
            "check dev-a",
            "--permission-mode",
            "bypassPermissions",
        ])
        .current_dir(&work_path)
        .env_clear()
        .env("HOME", home.path())
        .env("PATH", "/usr/bin:/bin")
        .env("SHELL", "/bin/bash")
        .env("ZCODE_DATA_BASE_DIR", home.path())
        .env("ZCODE_MODEL_BASE_URL", &fixture.url)
        .env("ZCODE_API_KEY", "test-key")
        .env("ZCODE_MODEL", GLM_5)
        .env("ZCODE_RUNTIME_ENV", "test")
        .env("_ZCODE_ASSUME_SSH_MACHINES", "1")
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
    assert_eq!(requests.len(), 3);
    // 工具定义带 `machine`，提醒写容器的系统与工作目录。
    let bash_schema = requests[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "Bash")
        .map(|tool| tool["input_schema"]["properties"]["machine"]["enum"].clone())
        .unwrap();
    assert_eq!(bash_schema, serde_json::json!(["local", "dev-a"]));
    let first = requests[0]["messages"].to_string();
    assert!(first.contains("- dev-a: Linux "), "{first}");
    assert!(first.contains("working directory /srv/proj"), "{first}");
    // 结果回到本机：命令在容器里、在 dev-a 的工作目录跑；相对路径的 Read 读容器里的文件。
    let last = |request: &Value| {
        request["messages"]
            .as_array()
            // 最后一条 user 消息（有卡片形状的模型把提醒放在其后的 system 消息里）。
            .and_then(|messages| {
                messages
                    .iter()
                    .rev()
                    .find(|message| message["role"] == "user")
            })
            .map(Value::to_string)
            .unwrap_or_default()
    };
    let ran = last(&requests[1]);
    assert!(ran.contains("Linux"), "{ran}");
    assert!(ran.contains("/srv/proj"), "{ran}");
    assert!(ran.contains("marker-42"), "{ran}");
    let read = last(&requests[2]);
    assert!(read.contains("from the container"), "{read}");

    // 收尾：停掉容器里的常驻执行器（容器随后删掉）。
    let installed = format!("/root/.zcode/rust-cli/exec/bin/{VERSION}/zcode-rs");
    let (ok, out, err) = sshd.ssh(&format!("{installed} executor stop"));
    assert!(ok, "{out}{err}");
}

/// 远端 MCP（R5，remote-exec.md §22.10）：`mcp-machines.json` 把一个 stdio 服务器（仓库里的 bash 夹具，复制进容器，不从
/// 网络装任何包）绑到 dev-a；真实的 `zcode -p`（只给测试的 SSH 入口）调用它的工具，结果来自容器里的文件与命令；配置里的
/// `${HOME}` 在容器里展开（本机的值不发过去）。
#[tokio::test(flavor = "multi_thread")]
async fn a_bound_mcp_server_runs_in_the_container() {
    let Some(artifact) = artifact_or_skip() else {
        return;
    };
    let sshd = Sshd::start();
    sshd.wait_ready().await;
    let (ok, _, err) = sshd.ssh(
        "mkdir -p /srv/proj /srv/fixtures && printf 'from the container\\n' > /srv/proj/notes.txt",
    );
    assert!(ok, "{err}");
    let fixture_script =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp-shell.sh");
    let into = format!("{}:/srv/fixtures/mcp-shell.sh", sshd.name);
    let (ok, _, err) = docker(&["cp", &fixture_script.to_string_lossy(), &into]);
    assert!(ok, "docker cp: {err}");
    let work = tempfile::tempdir().unwrap();
    let work_path = std::fs::canonicalize(work.path()).unwrap();
    let home = tempfile::tempdir().unwrap();
    let rust_cli = home.path().join(".zcode/rust-cli");
    std::fs::create_dir_all(&rust_cli).unwrap();
    std::fs::create_dir_all(home.path().join(".zcode/cli")).unwrap();
    let machines = serde_json::json!({"version": 1, "machines": [
        {"name": "dev-a", "ssh": HOST, "workingDirectory": "/srv/proj", "connected": true}
    ]});
    std::fs::write(rust_cli.join("machines.json"), machines.to_string()).unwrap();
    let bindings =
        serde_json::json!({"version": 1, "servers": {"browser-a": {"machine": "dev-a"}}});
    std::fs::write(rust_cli.join("mcp-machines.json"), bindings.to_string()).unwrap();
    let config = serde_json::json!({"mcp": {"servers": {"browser-a": {
        "type": "stdio",
        "command": "/bin/bash",
        "args": ["/srv/fixtures/mcp-shell.sh"],
        "env": {"MCP_PROBE": "${HOME}"},
        "alwaysLoad": true
    }}}});
    std::fs::write(
        home.path().join(".zcode/cli/config.json"),
        config.to_string(),
    )
    .unwrap();
    let fixture = sse::serve(vec![
        sse::tools_reply(&[sse::tool_block(0, "toolu_1", "mcp__browser-a__where", "{}")]),
        sse::text_reply("done"),
    ]);
    let output = Command::new(BIN)
        .args([
            "-p",
            "where is it",
            "--permission-mode",
            "bypassPermissions",
        ])
        .current_dir(&work_path)
        .env_clear()
        .env("HOME", home.path())
        .env("PATH", "/usr/bin:/bin")
        .env("SHELL", "/bin/bash")
        .env("ZCODE_DATA_BASE_DIR", home.path())
        .env("ZCODE_MODEL_BASE_URL", &fixture.url)
        .env("ZCODE_API_KEY", "test-key")
        .env("ZCODE_MODEL", GLM_5)
        .env("ZCODE_RUNTIME_ENV", "test")
        .env("_ZCODE_ASSUME_SSH_MACHINES", "1")
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
    assert_eq!(requests.len(), 2);
    let result = requests[1]["messages"]
        .as_array()
        // 最后一条 user 消息（有卡片形状的模型把提醒放在其后的 system 消息里）。
        .and_then(|messages| {
            messages
                .iter()
                .rev()
                .find(|message| message["role"] == "user")
        })
        .map(Value::to_string)
        .unwrap_or_default();
    assert!(result.contains("os=Linux"), "{result}");
    assert!(result.contains("cwd=/srv/proj"), "{result}");
    assert!(result.contains("notes=from the container"), "{result}");
    assert!(result.contains("probe=/root"), "{result}");
    assert!(
        !result.contains(&home.path().display().to_string()),
        "the local HOME reached the container: {result}"
    );

    // 收尾：停掉容器里的常驻执行器（它起的服务器随之结束；容器随后删掉）。
    let installed = format!("/root/.zcode/rust-cli/exec/bin/{VERSION}/zcode-rs");
    let (ok, out, err) = sshd.ssh(&format!("{installed} executor stop"));
    assert!(ok, "{out}{err}");
}
