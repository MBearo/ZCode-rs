//! `/diff`（docs/specs/rust-cli/tui-manage.md §4）：未提交的改动来自执行环境的 git 输出，回合的改动来自视图。
use std::sync::Arc;

use crossterm::event::KeyCode;
use rust_cli_kernel_api::env::{EnvFacts, EnvPath};
use rust_cli_kernel_api::env_git::GitDiff;
use rust_cli_kernel_api::testing::{
    MemoryEnvironment, environments, text_response, tool_use_response,
};
use serde_json::json;

use super::support::{GatedModel, Setup, with_ui};

fn facts() -> EnvFacts {
    EnvFacts {
        cwd: EnvPath::new("/Users/dev/project"),
        platform: "darwin".into(),
        os_version: "x".into(),
        shell: "zsh".into(),
        is_git_repo: true,
        additional_directories: Vec::new(),
        home: Some(EnvPath::new("/Users/dev")),
    }
}

fn idle(screen: &str, text: &str) -> bool {
    screen.contains(text) && !screen.contains("esc to interrupt")
}

#[tokio::test(flavor = "multi_thread")]
async fn uncommitted_changes_and_turns_are_listed() {
    let model = GatedModel::scripted(vec![
        tool_use_response(
            "toolu_1",
            "Write",
            &json!({"file_path": "/Users/dev/project/notes.md", "content": "alpha\nbeta\n"}),
        ),
        text_response(&["done"]),
    ]);
    let env = MemoryEnvironment::new(facts()).with_git_diff(GitDiff::Repository {
        numstat: "1\t1\tsrc/a.rs".into(),
        patch: "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-old line\n+new line".into(),
        untracked: "notes.md".into(),
    });
    let setup = Setup {
        width: 100,
        height: 30,
        environments: environments(Arc::new(env)),
        ..Setup::default()
    };
    with_ui(model, setup, |mut ui| async move {
        ui.submit("/mode yolo");
        // 工作线 F87：切到完全访问先确认。
        ui.confirm_full_access().await;
        ui.wait_for("⏵⏵ Full access").await;
        ui.submit("write the notes");
        ui.wait("the reply", |s| idle(s, "⏺ done")).await;
        ui.submit("/diff");
        let page = ui.wait_for("src/a.rs").await;
        assert!(
            page.contains("Uncommitted changes (git diff HEAD)"),
            "{page}"
        );
        assert!(
            page.contains("◀ Current · T1") || page.contains("Current · T1 ▶"),
            "{page}"
        );
        assert!(page.contains("2 files changed +1 -1"), "{page}");
        assert!(page.contains("› src/a.rs +1 -1"), "{page}");
        assert!(page.contains("notes.md untracked"), "{page}");
        // 详情：git 的 diff 行；esc 回到列表。
        ui.key(KeyCode::Enter);
        let detail = ui.wait_for("new line").await;
        assert!(detail.contains("old line"), "{detail}");
        assert!(detail.contains("↑/↓ to scroll · Esc to back"), "{detail}");
        ui.key(KeyCode::Esc);
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Enter);
        ui.wait_for("New file not yet staged.").await;
        ui.key(KeyCode::Esc);
        // 回合：这一回合里 Write 的内容。
        ui.key(KeyCode::Right);
        let turn = ui.wait_for("Turn 1 \"write the notes\"").await;
        assert!(turn.contains("notes.md +2"), "{turn}");
        ui.key(KeyCode::Enter);
        let detail = ui.wait_for("alpha").await;
        assert!(detail.contains("beta"), "{detail}");
        ui.key(KeyCode::Esc);
        ui.key(KeyCode::Esc);
        ui.wait_for("Diff dialog dismissed").await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn outside_a_repository_and_without_changes() {
    for (diff, text) in [
        (
            GitDiff::NotRepository,
            "the current directory isn't in a git repository",
        ),
        (
            GitDiff::Repository {
                numstat: String::new(),
                patch: String::new(),
                untracked: String::new(),
            },
            "No changes yet",
        ),
    ] {
        let model = GatedModel::scripted(Vec::new());
        let setup = Setup {
            width: 100,
            environments: environments(Arc::new(
                MemoryEnvironment::new(facts()).with_git_diff(diff),
            )),
            ..Setup::default()
        };
        with_ui(model, setup, |mut ui| async move {
            ui.wait_for("Ask before changes").await;
            ui.submit("/diff");
            let page = ui.wait_for(text).await;
            assert!(!page.contains("T1"), "{page}");
            ui.key(KeyCode::Esc);
            ui.wait_for("Diff dialog dismissed").await;
        })
        .await;
    }
}
