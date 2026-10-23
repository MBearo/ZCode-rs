//! `/permissions`（docs/specs/rust-cli/tui-manage.md §1）：加规则写进本机项目规则集，删除先确认，关闭时提示结果。
use std::sync::Arc;

use crossterm::event::KeyCode;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::store::SessionStore;
use rust_cli_kernel_api::testing::MemoryStore;

use super::support::{GatedModel, Setup, with_ui};

fn workspace() -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new("/Users/dev/project"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn rules_are_added_and_deleted_in_the_project_ruleset() {
    let model = GatedModel::scripted(Vec::new());
    let store = Arc::new(MemoryStore::default());
    let setup = Setup {
        store: store.clone(),
        ..Setup::default()
    };
    let check = store.clone();
    with_ui(model.clone(), setup, |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("/permissions");
        let page = ui.wait_for("Permissions:").await;
        assert!(
            page.contains("ZCode won't ask before using allowed tools."),
            "{page}"
        );
        assert!(page.contains("› Add a new rule…"), "{page}");
        ui.key(KeyCode::Enter);
        ui.wait_for("Add allow permission rule").await;
        ui.type_text("Bash(ls *)");
        ui.key(KeyCode::Enter);
        let destination = ui.wait_for("Where should this rule be saved?").await;
        assert!(
            destination.contains("Any Bash command starting with ls"),
            "{destination}"
        );
        ui.key(KeyCode::Enter);
        ui.wait("the new rule in the list", |screen| {
            screen.contains("Permissions:") && screen.contains("    Bash(ls *)")
        })
        .await;
        let saved = check.project_rules(&workspace()).await.unwrap();
        assert_eq!(saved.allow.len(), 1);
        assert_eq!(saved.allow[0].content.as_deref(), Some("ls *"));
        // 删除：先确认（焦点在 No），选 Yes 后从规则集里去掉。
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Enter);
        ui.wait_for("Delete allowed tool?").await;
        ui.key(KeyCode::Up);
        ui.key(KeyCode::Enter);
        ui.wait("the rule to go away", |screen| {
            screen.contains("Permissions:") && !screen.contains("Bash(ls *)")
        })
        .await;
        assert!(
            check
                .project_rules(&workspace())
                .await
                .unwrap()
                .allow
                .is_empty()
        );
        ui.key(KeyCode::Esc);
        let screen = ui.wait_for("Deleted allow rule Bash(ls *)").await;
        assert!(screen.contains("Added allow rule Bash(ls *)"), "{screen}");
        // 什么都没改就关闭：提示已关闭。
        ui.submit("/allowed-tools");
        ui.wait_for("Permissions:").await;
        ui.key(KeyCode::Esc);
        ui.wait_for("Permissions dialog dismissed").await;
    })
    .await;
    assert!(model.requests().is_empty());
}
