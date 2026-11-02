//! 收按键、粘贴与画在最上面的是同一层（架构整理 U7，用户决定 Q8，tui.md §7）：视图 → 待回答的交互 → 同伴消息的批准框
//! → 输入框。
use std::time::{Duration, Instant};

use crossterm::event::KeyCode;
use rust_cli_kernel_api::ids::{InteractionId, SessionId, ToolUseId};
use rust_cli_kernel_api::orchestration::{DetachedAsk, DetachedEvent, InteractionRequest};
use rust_cli_kernel_api::permission::PermissionRule;
use serde_json::json;

use crate::ui::app::{App, Overlay};
use crate::ui::export::ExportView;
use crate::ui::test_support;
use crate::ui::test_support::key;

fn app() -> App {
    let mut app = test_support::app();
    app.composer.skills_loaded = true;
    app
}

/// 一个待回答的 Bash 权限询问（旧会话的询问，与当前会话的询问同一条显示与按键路径）。
fn ask(app: &mut App, suggestions: Vec<PermissionRule>) {
    app.detached_changed(DetachedEvent::Asked {
        ask: Box::new(DetachedAsk {
            interaction: InteractionId::new("i1"),
            session: SessionId::new("sess_old"),
            title: String::new(),
            request: InteractionRequest::Permission {
                tool_use: ToolUseId::new("tu1"),
                tool: "Bash".into(),
                input: json!({"command": "npm run test"}),
                message: "needs approval".into(),
                suggestions,
                rule_seed: None,
                asked_by: None,
                machine: Default::default(),
            },
        }),
    });
    // 对话框弹出 150 ms 之内的按键不生效：之后再按。
    app.sync_interaction(Instant::now() - Duration::from_secs(1));
}

/// 修前：按键先给视图，粘贴先给交互。异步的结果打开了带输入框的视图（这里是 `/export` 的文件名），下面还有一个正在写
/// 意见的询问框时，按键进视图，粘贴进了被盖住的询问框。
#[test]
fn a_paste_goes_to_the_view_that_covers_a_typing_dialog() {
    let mut app = app();
    ask(&mut app, Vec::new());
    // tab：写拒绝的意见。
    app.on_key(key(KeyCode::Tab), Instant::now());
    assert!(app.interaction.as_ref().unwrap().typing);
    app.overlay = Some(Overlay::Export(ExportView::Name(String::new())));
    app.on_paste("notes.md");
    assert!(
        matches!(&app.overlay, Some(Overlay::Export(ExportView::Name(name))) if name == "notes.md"),
        "the paste goes where the keys go"
    );
    assert_eq!(app.interaction.as_ref().unwrap().text, "");
    // 按键同样进视图。
    app.on_key(key(KeyCode::Char('x')), Instant::now());
    assert!(
        matches!(&app.overlay, Some(Overlay::Export(ExportView::Name(name))) if name == "notes.mdx")
    );
}

/// 修前：询问框盖住输入框、没有在写意见时，粘贴进了看不见的输入框（按键不会到那里）。
#[test]
fn a_paste_under_a_dialog_does_not_reach_the_hidden_input_box() {
    let mut app = app();
    ask(&mut app, Vec::new());
    app.on_paste("hello");
    assert!(app.composer.is_empty(), "{:?}", app.composer.area.text());
}

/// 修前：焦点在“不再询问”一行的可编辑前缀上时，按键改前缀，粘贴却进了看不见的输入框。
#[test]
fn a_paste_on_the_editable_rule_goes_into_the_rule() {
    let mut app = app();
    ask(
        &mut app,
        vec![PermissionRule {
            tool: "Bash".into(),
            content: Some("npm run test:*".into()),
        }],
    );
    let ui = app.interaction.as_mut().unwrap();
    ui.seed = Some("npm run".into());
    ui.cursor = 1;
    app.on_paste(" test");
    assert!(app.composer.is_empty(), "{:?}", app.composer.area.text());
    assert_eq!(
        app.interaction.as_ref().unwrap().seed.as_deref(),
        Some("npm run test")
    );
}

/// 快捷键面板画在输入框上面，属于输入框这一层：面板开着时来了询问，画面上是询问框，按键也给询问框。修前第一个键先
/// 关掉了看不见的面板（键被吞掉）。
#[test]
fn a_dialog_that_arrives_over_the_shortcuts_gets_the_keys() {
    let mut app = app();
    app.overlay = Some(Overlay::Shortcuts);
    ask(&mut app, Vec::new());
    let action = app.on_key(key(KeyCode::Char('1')), Instant::now());
    assert!(
        matches!(action, Some(crate::ui::app::Action::Resolve { .. })),
        "{action:?}"
    );
}
