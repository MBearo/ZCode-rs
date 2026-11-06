//! `/export` 的缺省文件名与文字（tui-manage.md §3.1）。
use super::*;

/// 2026-10-18 09:05:07 UTC。
const AT: u64 = 1_792_314_307_000;

#[test]
fn the_default_name_is_the_time_and_a_slug() {
    assert_eq!(timestamp(AT, 0), "2026-10-18-090507");
    assert_eq!(
        default_name(Some("  Fix the README, please!  "), AT, 0),
        "2026-10-18-090507-fix-the-readme-please.txt"
    );
    assert_eq!(
        default_name(Some("a -- b\tc"), AT, 0),
        "2026-10-18-090507-a-b-c.txt"
    );
    // 超过 50 个字符：取前 49 个，省略号被 slug 去掉。
    let long = "word ".repeat(20);
    let name = default_name(Some(&long), AT, 0);
    assert_eq!(
        name.len(),
        "2026-10-18-090507-".len() + 49 + ".txt".len(),
        "{name}"
    );
    assert_eq!(
        default_name(Some("中文 输入"), AT, 0),
        "conversation-2026-10-18-090507.txt"
    );
    assert_eq!(
        default_name(None, AT, 0),
        "conversation-2026-10-18-090507.txt"
    );
}

/// 修复（R2-6）：以前缺省文件名按 UTC（tui-manage.md D9）；现在用本地时间，偏移取入口注入的时钟。
#[test]
fn the_default_name_uses_the_local_time() {
    assert_eq!(timestamp(AT, 8 * 60), "2026-10-18-170507");
    assert_eq!(timestamp(AT, -10 * 60), "2026-10-17-230507");
    assert_eq!(
        default_name(Some("fix it"), AT, 8 * 60),
        "2026-10-18-170507-fix-it.txt"
    );
}

fn app() -> App {
    use rust_cli_kernel_api::permission::PermissionMode;
    App::new(
        crate::ui::probe::Colors::new(crate::ui::probe::ColorLevel::TrueColor, Some((0, 0, 0))),
        Lang::En,
        crate::view::Transcript::new(
            rust_cli_kernel_api::testing::selection(),
            PermissionMode::Default,
            crate::view::test_workspace(),
        ),
        Some("/Users/dev".into()),
        "0.1.0".into(),
    )
}

/// `/export`（tui-kit.md §4.2）：跟着草稿的列表，草稿是 `/export `，两行是 Puffin 的名字与说明；选「存到文件」时草稿变成
/// `/export <缺省文件名>`、列表关上。以前是自己的对话框，文件名在门道里填。
#[test]
fn export_opens_puffins_list_in_the_draft_and_the_file_name_goes_into_the_draft() {
    let mut app = app();
    assert!(app.export_command("").is_none());
    assert_eq!(app.composer.area.text(), EXPORT_DRAFT);
    let Some(crate::ui::app::Overlay::List(list)) = &app.overlay else {
        panic!("no list: {:?}", app.overlay.is_some());
    };
    assert_eq!(list.purpose, Purpose::Export);
    let names: Vec<(&str, &str)> = list
        .items
        .iter()
        .map(|item| (item.label.as_str(), item.description.as_str()))
        .collect();
    assert_eq!(
        names,
        rust_cli_tui_kit::app::widgets::export_list::EXPORT_ROWS
    );
    app.open_export_name(AT, 0);
    assert_eq!(
        app.composer.area.text(),
        "/export conversation-2026-10-18-090507.txt"
    );
    // 带文件名时直接写。
    assert!(matches!(
        app.export_command(" chat.txt "),
        Some(Action::Manage(Manage::Export(name))) if name == "chat.txt"
    ));
}

/// 结果的句子（Puffin）：家目录写成 `~`。
#[test]
fn the_result_names_the_path_from_home() {
    let app = app();
    assert_eq!(
        exported(Lang::En, &app.tilde("/Users/dev/project/chat.txt")),
        "Exported to ~/project/chat.txt."
    );
    assert_eq!(app.tilde("/Users/devx/a"), "/Users/devx/a");
    assert_eq!(
        export_failed(Lang::En, "denied"),
        "Could not export: denied"
    );
}
