//! `/export`（docs/specs/rust-cli/tui-manage.md §3）：写文件经内核与执行环境，复制经 `Screen::copy`。
use std::sync::Arc;

use crossterm::event::KeyCode;
use rust_cli_kernel_api::env::{EnvFacts, EnvPath, Environment, ReadFile, WorkspaceRef};
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::testing::{MemoryEnvironment, text_response};

use super::support::{GatedModel, Setup, with_ui};

fn env() -> Arc<MemoryEnvironment> {
    Arc::new(MemoryEnvironment::new(EnvFacts {
        cwd: EnvPath::new("/Users/dev/project"),
        platform: "darwin".into(),
        os_version: "x".into(),
        shell: "zsh".into(),
        is_git_repo: false,
        additional_directories: Vec::new(),
        home: Some(EnvPath::new("/Users/dev")),
    }))
}

async fn read(env: &MemoryEnvironment, path: &str) -> String {
    let workspace = WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new("/Users/dev/project"),
    };
    let file = env
        .read_file(
            &workspace,
            ReadFile {
                path: EnvPath::new(path),
            },
        )
        .await
        .unwrap();
    String::from_utf8(file.bytes).unwrap()
}

fn idle(screen: &str, text: &str) -> bool {
    screen.contains(text) && !screen.contains("esc to interrupt")
}

#[tokio::test(flavor = "multi_thread")]
async fn the_conversation_goes_to_a_file_or_the_clipboard() {
    let model = GatedModel::scripted(vec![text_response(&["world"])]);
    let env = env();
    let setup = Setup {
        env: env.clone(),
        ..Setup::default()
    };
    let check = env.clone();
    let named = Arc::new(std::sync::Mutex::new(String::new()));
    let found = named.clone();
    let copies = Arc::new(std::sync::Mutex::new(Vec::new()));
    let copied = copies.clone();
    with_ui(model, setup, |mut ui| async move {
        ui.submit("hello there");
        ui.wait("the reply", |s| idle(s, "⏺ world")).await;
        ui.submit("/export notes/chat");
        ui.wait_for("Conversation exported to: /Users/dev/project/notes/chat.txt")
            .await;
        let text = read(&check, "/Users/dev/project/notes/chat.txt").await;
        assert!(text.contains("❯ hello there"), "{text}");
        assert!(text.contains("⏺ world"), "{text}");
        assert!(text.ends_with('\n') && !text.contains(" \n"), "{text:?}");
        // 对话框：复制到剪贴板。
        ui.submit("/export");
        let dialog = ui.wait_for("Select export method").await;
        assert!(dialog.contains("› 1. Copy to clipboard"), "{dialog}");
        ui.key(KeyCode::Enter);
        ui.wait_for("Conversation copied to clipboard").await;
        copied
            .lock()
            .unwrap()
            .extend(ui.copies.lock().unwrap().clone());
        // 保存到文件：预填缺省文件名。
        ui.submit("/export");
        ui.wait_for("Select export method").await;
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Enter);
        let input = ui.wait_for("Enter filename:").await;
        let line = input
            .lines()
            .find(|line| line.trim_start().starts_with("> "))
            .unwrap()
            .trim()
            .trim_start_matches("> ")
            .trim_end_matches('█')
            .to_owned();
        assert!(line.ends_with("-hello-there.txt"), "{line}");
        *found.lock().unwrap() = line;
        ui.key(KeyCode::Enter);
        ui.wait_for("-hello-there.txt").await;
        ui.wait("the second export", |s| {
            s.matches("Conversation exported to:").count() == 2
        })
        .await;
        // 取消。
        ui.submit("/export");
        ui.wait_for("Select export method").await;
        ui.key(KeyCode::Esc);
        ui.wait_for("Export cancelled").await;
    })
    .await;
    let copies = copies.lock().unwrap().clone();
    assert_eq!(copies.len(), 1);
    assert!(copies[0].contains("❯ hello there"), "{}", copies[0]);
    let name = named.lock().unwrap().clone();
    let text = read(&env, &format!("/Users/dev/project/{name}")).await;
    assert!(text.contains("⏺ world"));
}
