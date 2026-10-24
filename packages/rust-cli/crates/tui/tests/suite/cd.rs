//! `/cd`（docs/specs/rust-cli/tui-manage.md §7）：会话换到新目录，下一个请求带说明与新目录的规则文件，之后的
//! `/export` 写到新目录；各种错误按固定的文字。
use std::sync::Arc;

use rust_cli_kernel_api::env::{EnvFacts, EnvPath, ReadFile, WorkspaceRef};
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::machine::Files;
use rust_cli_kernel_api::testing::{MemoryEnvironment, environments, text_response};

use super::support::{GatedModel, Setup, request_text, with_ui};

fn env() -> Arc<MemoryEnvironment> {
    Arc::new(
        MemoryEnvironment::new(EnvFacts {
            cwd: EnvPath::new("/Users/dev/project"),
            platform: "darwin".into(),
            os_version: "x".into(),
            shell: "zsh".into(),
            is_git_repo: false,
            additional_directories: Vec::new(),
            home: Some(EnvPath::new("/Users/dev")),
        })
        .with_file("/Users/dev/project/AGENTS.md", "project rules")
        .with_file("/Users/dev/other/AGENTS.md", "other rules")
        .with_file("/Users/dev/notes.txt", "x"),
    )
}

fn idle(screen: &str, text: &str) -> bool {
    screen.contains(text) && !screen.contains("esc to interrupt")
}

#[tokio::test(flavor = "multi_thread")]
async fn the_session_moves_and_the_next_request_knows() {
    let model = GatedModel::scripted(vec![text_response(&["first"]), text_response(&["second"])]);
    let env = env();
    let setup = Setup {
        width: 100,
        environments: environments(env.clone()),
        ..Setup::default()
    };
    with_ui(model.clone(), setup, |mut ui| async move {
        ui.submit("/cd");
        ui.wait_for("Usage: /cd <path>").await;
        ui.submit("hello");
        ui.wait("the first reply", |s| idle(s, "⏺ first")).await;
        ui.submit("/cd ../missing");
        ui.wait_for("Couldn't find a directory at /Users/dev/missing.")
            .await;
        ui.submit("/cd ~/notes.txt");
        ui.wait_for("/Users/dev/notes.txt is not a directory. Did you mean /Users/dev?")
            .await;
        ui.submit("/cd .");
        ui.wait_for("Already in ~/project.").await;
        ui.submit("/cd ../other");
        ui.wait_for("Moved to ~/other").await;
        ui.submit("next");
        ui.wait("the second reply", |s| idle(s, "⏺ second")).await;
        ui.submit("/export chat");
        ui.wait_for("Conversation exported to: /Users/dev/other/chat.txt")
            .await;
    })
    .await;
    let requests = model.requests();
    assert_eq!(requests.len(), 2);
    let second = request_text(&requests[1]);
    assert!(
        second
            .contains("The session's working directory has changed to /Users/dev/other (via /cd)."),
        "{second}"
    );
    assert!(second.contains("other rules"), "{second}");
    assert!(!request_text(&requests[0]).contains("via /cd"));
    let workspace = WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new("/Users/dev/other"),
    };
    let exported = env
        .read_file(
            &workspace,
            ReadFile {
                path: EnvPath::new("/Users/dev/other/chat.txt"),
            },
        )
        .await
        .unwrap();
    assert!(
        String::from_utf8(exported.bytes)
            .unwrap()
            .contains("⏺ second")
    );
}

/// 复现（R1-3）：以前 TUI 自存一份工作区，只有 `/cd` 更新它。会话关闭后再打开回到创建时的目录（tools-more.md
/// D23），TUI 的副本还停在 `/cd` 去的目录，之后的 `/export` 写错地方。现在 TUI 的工作区只从快照与
/// `WorkspaceChanged` 取。
#[tokio::test(flavor = "multi_thread")]
async fn the_reopened_session_brings_its_own_workspace() {
    use crossterm::event::KeyCode;

    let model = GatedModel::scripted(vec![text_response(&["first"])]);
    let env = env();
    let setup = Setup {
        width: 100,
        environments: environments(env.clone()),
        ..Setup::default()
    };
    with_ui(model, setup, |mut ui| async move {
        ui.submit("hello");
        ui.wait("the first reply", |s| idle(s, "⏺ first")).await;
        ui.submit("/cd ../other");
        ui.wait_for("Moved to ~/other").await;
        // 切回同一个会话：`/clear` 关闭它（离开内核），`/resume` 页（修复线 F29）的 Filter 换到 All 才看得到它（当前
        // 目录已是 ~/other）；选它时目录不同，用会话目录——冷加载后工作目录回到创建时的 ~/project。
        ui.submit("/clear");
        ui.wait_for("Started a new session.").await;
        ui.submit("/resume");
        ui.wait_for("Type to search").await;
        ui.key(KeyCode::Right);
        ui.wait_for("⌁ ~/project").await;
        ui.key(KeyCode::Enter);
        ui.wait_for("Working directory · resume").await;
        ui.key(KeyCode::Enter);
        ui.wait("the session to reopen", |s| {
            s.contains("⏺ first") && !s.contains("Working directory")
        })
        .await;
        ui.submit("/export chat");
        ui.wait_for("Conversation exported to: /Users/dev/project/chat.txt")
            .await;
    })
    .await;
}
