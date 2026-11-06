//! `/export`（docs/specs/rust-cli/tui-manage.md §3）：写文件经内核与执行环境，复制经 `Screen::copy`。
use std::sync::Arc;

use crossterm::event::KeyCode;
use rust_cli_kernel_api::env::{EnvFacts, EnvPath, ReadFile, WorkspaceRef};
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::machine::Files;
use rust_cli_kernel_api::testing::{MemoryEnvironment, environments, text_response};

use super::support::{GatedModel, Setup, with_ui};

fn env() -> Arc<MemoryEnvironment> {
    Arc::new(MemoryEnvironment::new(EnvFacts {
        cwd: EnvPath::new("/Users/dev/project"),
        platform: "darwin".into(),
        os_version: "x".into(),
        shell: "zsh".into(),
        is_git_repo: false,
        home: Some(EnvPath::new("/Users/dev")),
        uid: None,
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
    screen.contains(text) && !screen.contains("esc stop")
}

#[tokio::test(flavor = "multi_thread")]
async fn the_conversation_goes_to_a_file_or_the_clipboard() {
    let model = GatedModel::scripted(vec![text_response(&["world"])]);
    let env = env();
    let setup = Setup {
        environments: environments(env.clone()),
        ..Setup::default()
    };
    let check = env.clone();
    let named = Arc::new(std::sync::Mutex::new(String::new()));
    let found = named.clone();
    let copies = Arc::new(std::sync::Mutex::new(Vec::new()));
    let copied = copies.clone();
    with_ui(model, setup, |mut ui| async move {
        ui.submit("hello there");
        ui.wait("the reply", |s| idle(s, "   world")).await;
        ui.submit("/export notes/chat");
        ui.wait_for("Exported to ~/project/notes/chat.txt").await;
        let text = read(&check, "/Users/dev/project/notes/chat.txt").await;
        assert!(text.contains(" › hello there"), "{text}");
        assert!(text.contains("   world"), "{text}");
        assert!(text.ends_with('\n') && !text.contains(" \n"), "{text:?}");
        // Puffin 的 `/export` 层（tui-kit.md §4.2）：两行方式，从第 3 列起，没有头一行、序号与指针。以前有
        // `Select export method` 的头、`Enter filename:` 的输入行与 `Export cancelled`。
        const SAVE: &str = "Save the conversation to a file in this folder";
        ui.submit("/export");
        let dialog = ui.wait_for(SAVE).await;
        assert!(
            dialog
                .lines()
                .any(|l| l.trim_start().starts_with("Copy to clipboard")),
            "{dialog}"
        );
        // 复制：反馈同拖选（Puffin `copied_says`）。
        ui.key(KeyCode::Enter);
        ui.wait("the copy", |s| s.contains("Copied ") && !s.contains(SAVE))
            .await;
        copied
            .lock()
            .unwrap()
            .extend(ui.copies.lock().unwrap().clone());
        // 保存到文件：草稿变成 `/export <缺省文件名>`，⏎ 存。
        ui.clear();
        ui.submit("/export");
        ui.wait_for(SAVE).await;
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Enter);
        let input = ui
            .wait("the default name", |s| s.contains("-hello-there.txt"))
            .await;
        let line = input
            .lines()
            .find(|line| line.trim_start().starts_with("› /export "))
            .unwrap()
            .trim()
            .trim_start_matches("› /export ")
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_owned();
        assert!(line.ends_with("-hello-there.txt"), "{line}");
        *found.lock().unwrap() = line;
        ui.key(KeyCode::Enter);
        // 结果在输入框上方安静的一行，到下一次发送为止（tui-kit.md §4.0「本地命令的结果」）。
        let screen = ui.wait_for("Exported to ").await;
        assert!(screen.contains("-hello-there.t"), "{screen}");
        assert_eq!(screen.matches("Exported to ").count(), 1, "{screen}");
        // esc 关上列表，草稿留着 `/export`（Puffin 的板）。
        ui.submit("/export");
        ui.wait_for(SAVE).await;
        ui.key(KeyCode::Esc);
        ui.wait("the list to close", |s| {
            !s.contains(SAVE) && s.contains("› /export")
        })
        .await;
    })
    .await;
    let copies = copies.lock().unwrap().clone();
    assert_eq!(copies.len(), 1);
    assert!(copies[0].contains(" › hello there"), "{}", copies[0]);
    let name = named.lock().unwrap().clone();
    let text = read(&env, &format!("/Users/dev/project/{name}")).await;
    assert!(text.contains("   world"));
}
