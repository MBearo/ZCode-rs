//! `/permissions`（docs/specs/rust-cli/tui-manage.md §1；tui-kit.md §4.2「设置」）：打开设置的 Permissions 段；新规则写进本机
//! 项目规则集，本项目的规则能删。以前是一个自己的列表（Allow / Ask / Deny 三个标签页、确认框、关闭时的提示），照 Puffin 并进
//! 设置页。
use std::sync::Arc;

use crossterm::event::KeyCode;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::store::LocalSettings;
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
        width: 120,
        height: 30,
        ..Setup::default()
    };
    let check = store.clone();
    with_ui(model.clone(), setup, |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("/permissions");
        let page = ui.wait_for("Settings  ›  Permissions").await;
        assert!(page.contains("+ Add a rule"), "{page}");
        // `+ Add a rule` 打开新规则的页：Rule 一行改成打的字，下面跟着它存成什么、意味着什么。
        ui.key(KeyCode::Enter);
        ui.wait_for("Settings  ›  Permissions  ›  New rule").await;
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Enter);
        ui.type_text("Bash(ls *)");
        ui.key(KeyCode::Enter);
        ui.wait_for("commands that start with ls").await;
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Enter);
        let list = ui
            .wait_for("Added. The agent uses it from its next tool call.")
            .await;
        assert!(
            list.contains("Allows any command that starts with ls"),
            "{list}"
        );
        let saved = check.project_rules(&workspace()).await.unwrap();
        assert_eq!(saved.allow.len(), 1);
        assert_eq!(saved.allow[0].content.as_deref(), Some("ls *"));
        // 选中的是新规则：⌫ 在它下面问 Remove / Keep it（Puffin 的就地确认，缺省 Keep it），选 Remove 后从规则集里去掉。
        assert!(list.contains("⌫ remove"), "{list}");
        ui.key(KeyCode::Backspace);
        ui.wait_for("The agent will ask before ls again.").await;
        ui.key(KeyCode::Up);
        ui.key(KeyCode::Enter);
        ui.wait("the rule to go away", |screen| {
            screen.contains("Nothing here yet.") && !screen.contains("Keep it")
        })
        .await;
        for _ in 0..200 {
            if check
                .project_rules(&workspace())
                .await
                .unwrap()
                .allow
                .is_empty()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            check
                .project_rules(&workspace())
                .await
                .unwrap()
                .allow
                .is_empty()
        );
        ui.key(KeyCode::Esc);
        ui.wait("the page to close", |s| !s.contains("Settings  ›"))
            .await;
    })
    .await;
    assert!(model.requests().is_empty());
}
