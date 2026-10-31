//! 配置文件与项目 hooks 信任的真实进程（docs/specs/rust-cli/config.md §4.3、§4.4）：临时 home 与工作区，
//! 本地假的 Anthropic 端点。未信任的项目 hook 不运行、`-p` 结束时写 Node 的提示；`zcode hooks trust
//! grant` 之后运行。用户级 hooks 不需要审核。新会话的模式按 Node 的顺序取（config.md §2.2）。
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录、起本地 HTTP 夹具、检查写出的文件"
)]

mod system_env;
use rust_cli_base::models::GLM_5_3;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Output, Stdio};
use system_env::KeepSystem as _;

const REPLY: &str = concat!(
    "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",",
    "\"role\":\"assistant\",\"model\":\"m\",\"content\":[],\"stop_reason\":null,\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n",
    "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"done\"}}\n\n",
    "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
);

/// 每个连接回同一段 SSE 文本回复。
fn model_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0; length];
            let _ = reader.read_exact(&mut body);
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                REPLY.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(REPLY.as_bytes());
        }
    });
    format!("http://{address}")
}

fn zcode(home: &Path, work: &Path, args: &[&str], server: &str) -> Output {
    let mut command = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"));
    command
        .args(args)
        .current_dir(work)
        .env_clear()
        .keep_system()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("PATH", "/usr/bin:/bin")
        .env("ZCODE_DATA_BASE_DIR", home)
        .env("ZCODE_MODEL_BASE_URL", server)
        .env("ZCODE_API_KEY", "test-key")
        .env("ZCODE_MODEL", GLM_5_3)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn write(path: &Path, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

/// hook 是 shell 命令（`/usr/bin/touch`）：只在 Unix 上跑（F66 起本文件其余部分三平台都跑）。
#[cfg(unix)]
#[test]
fn untrusted_project_hooks_are_skipped_until_granted() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (home, work) = (base.join("home"), base.join("work"));
    std::fs::create_dir_all(work.join(".git")).unwrap();
    // 用户级 hook 不需要审核；项目级 hook 要先授权。两个都在 UserPromptSubmit 时写一个标记文件。
    write(
        &home.join(".zcode/cli/config.json"),
        r#"{"hooks": {"enabled": true, "events": {"UserPromptSubmit": [{"hooks": [
            {"type": "command", "command": "echo user > \"$ZCODE_PROJECT_DIR/user.marker\""}]}]}}}"#,
    );
    write(
        &work.join(".zcode/config.json"),
        r#"{"hooks": {"events": {"UserPromptSubmit": [{"hooks": [
            {"type": "process", "command": "/usr/bin/touch", "args": ["${ZCODE_PROJECT_DIR}/project.marker"]}]}]}}}"#,
    );
    let server = model_server();

    let first = zcode(&home, &work, &["-p", "hello"], &server);
    assert!(first.status.success(), "{}", text(&first.stderr));
    assert_eq!(text(&first.stdout).trim(), "done");
    assert!(work.join("user.marker").exists());
    assert!(!work.join("project.marker").exists());
    let stderr = text(&first.stderr);
    let workspace = work.to_string_lossy();
    assert!(
        stderr.contains(&format!(
            "Workspace Hooks skipped: workspace_hooks_pending_trust\nworkspace: {workspace}\n"
        )),
        "{stderr}"
    );
    assert!(stderr.contains(&format!(
        "Review with: zcode hooks trust review --workspace \"{workspace}\"\n"
    )));

    let review = zcode(
        &home,
        &work,
        &["hooks", "trust", "review", "--json"],
        &server,
    );
    let status: serde_json::Value = serde_json::from_slice(&review.stdout).unwrap();
    assert_eq!(status["reasonCode"], "workspace_hooks_pending_trust");
    let bundle = status["bundleDigest"].as_str().unwrap().to_owned();
    let grant = zcode(
        &home,
        &work,
        &[
            "hooks",
            "trust",
            "grant",
            "--all-current",
            "--bundle-digest",
            &bundle,
        ],
        &server,
    );
    assert!(grant.status.success(), "{}", text(&grant.stderr));
    assert!(text(&grant.stdout).contains("state: workspace_hooks_trusted_persistent"));
    assert!(
        home.join(".zcode/security/workspace-hook-trust-v1.json")
            .exists()
    );

    let second = zcode(&home, &work, &["-p", "hello again"], &server);
    assert!(second.status.success(), "{}", text(&second.stderr));
    assert!(work.join("project.marker").exists());
    assert!(!text(&second.stderr).contains("Workspace Hooks skipped"));
}

/// 工作区最近一个会话记下的模式（与入口相同的存储位置）。
async fn latest_mode(
    home: &Path,
    work: &Path,
) -> Option<rust_cli_kernel_api::permission::PermissionMode> {
    use rust_cli_kernel_api::store::{SessionFilter, SessionStore};
    let paths = rust_cli_paths::StoragePaths::resolve(&Default::default(), home, work);
    let store = rust_cli_node_store::NodeStore::new(
        paths,
        std::sync::Arc::new(rust_cli_node_store::NodeIds::new(std::sync::Arc::new(
            rust_cli_node_store::SystemClock::utc(),
        ))),
    );
    let filter = SessionFilter {
        workspace: workspace(work),
        limit: 1,
    };
    let latest = store.list(filter).await.unwrap().remove(0);
    let loaded = store.load(&latest.id).await.unwrap();
    // 判决用的模式（修复线 F20：plan 是开关，`--permission-mode plan` 是 plan 开、default）。
    let mode = loaded.mode?;
    Some(rust_cli_kernel_api::permission::ExecutionState::from_parts(mode, loaded.plan).effective())
}

fn workspace(work: &Path) -> rust_cli_kernel_api::env::WorkspaceRef {
    rust_cli_kernel_api::env::WorkspaceRef {
        environment: rust_cli_kernel_api::ids::EnvironmentId::new("local"),
        root: rust_cli_kernel_api::env::EnvPath::new(work.to_string_lossy()),
    }
}

/// 本机项目的持久模式（config.md §2.2）：盖过配置文件的 `permission.mode`，命令行的 `--permission-mode`
/// 又盖过它。持久模式由 node-store 写进库里（与用户在 TUI 里切换模式时同一个事实）。
#[tokio::test]
async fn the_project_mode_sits_between_the_command_line_and_the_config() {
    use rust_cli_kernel_api::permission::PermissionMode;
    use rust_cli_kernel_api::store::{Fact, FactBatch, SessionFilter, SessionStore};
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (home, work) = (base.join("home"), base.join("work"));
    std::fs::create_dir_all(work.join(".git")).unwrap();
    write(
        &home.join(".zcode/cli/config.json"),
        r#"{"permission": {"mode": "yolo"}}"#,
    );
    let server = model_server();
    let run = |args: &[&str]| {
        let output = zcode(&home, &work, args, &server);
        assert!(output.status.success(), "{}", text(&output.stderr));
    };
    run(&["-p", "hello"]);
    assert_eq!(
        latest_mode(&home, &work).await,
        Some(PermissionMode::BypassPermissions)
    );
    // 像用户切换到 acceptEdits 那样写持久模式。
    let paths = rust_cli_paths::StoragePaths::resolve(&Default::default(), &home, &work);
    let store = rust_cli_node_store::NodeStore::new(
        paths,
        std::sync::Arc::new(rust_cli_node_store::NodeIds::new(std::sync::Arc::new(
            rust_cli_node_store::SystemClock::utc(),
        ))),
    );
    let filter = SessionFilter {
        workspace: workspace(&work),
        limit: 1,
    };
    let session = store.list(filter).await.unwrap().remove(0).id;
    let batch = FactBatch {
        owner_epoch: rust_cli_kernel_api::ids::OwnerEpoch::LOCAL,
        command: None,
        facts: vec![Fact::ProjectModeSet {
            mode: PermissionMode::AcceptEdits,
        }],
        turn: None,
    };
    store.commit(&session, batch).await.unwrap();
    drop(store);
    run(&["-p", "again"]);
    assert_eq!(
        latest_mode(&home, &work).await,
        Some(PermissionMode::AcceptEdits)
    );
    run(&["-p", "planned", "--permission-mode", "plan"]);
    assert_eq!(latest_mode(&home, &work).await, Some(PermissionMode::Plan));
}
