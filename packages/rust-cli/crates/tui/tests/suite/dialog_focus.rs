//! 权限询问与计划审批的焦点行（docs/specs/rust-cli/tui.md §7.7，工作线 F76）。样子照 Puffin 的「请求」与「计划」
//! （tui.md §5.2，tui-kit.md §4.2）：回答在 panel 底色的板上，焦点行的序号与名字是 brand、不加粗，其余序号 fg3、名字 fg2，没有
//! 底色条；意见在真的输入框里打，打了字时选中 Deny / Keep planning，⏎ 带着字发出。深浅两种背景。扣住的同伴消息在 peers.rs。
use std::sync::Arc;

use super::support::Shot;
use crossterm::event::KeyCode;
use rust_cli_kernel_api::env::{EnvFacts, EnvPath};
use rust_cli_kernel_api::testing::{
    MemoryEnvironment, environments, text_response, tool_use_response,
};
use rust_cli_tui::{ColorLevel, Colors, Ink};
use rust_cli_tui_kit::render::{Color, attr};
use serde_json::json;

use super::support::{GatedModel, Setup, Ui, request_text, with_ui};

type Rgb = (u8, u8, u8);

struct Theme {
    background: Rgb,
    foreground: Rgb,
}

/// 期望的墨（主题 token，tui-kit.md §3.4）。
#[derive(Clone, Copy)]
struct Inks {
    brand: Color,
    fg2: Color,
    fg3: Color,
    panel: Color,
}

fn inks(theme: &Theme) -> Inks {
    let ink = Ink::new(
        ColorLevel::TrueColor,
        Some(theme.foreground),
        Some(theme.background),
    );
    let theme = ink.theme;
    let Some(panel) = theme.panel else {
        panic!("rich 的主题有板的底色");
    };
    Inks {
        brand: theme.brand,
        fg2: theme.fg2.fg_color(),
        fg3: theme.fg3.fg_color(),
        panel,
    }
}

const THEMES: [Theme; 2] = [
    Theme {
        background: (0, 0, 0),
        foreground: (255, 255, 255),
    },
    Theme {
        background: (255, 255, 255),
        foreground: (0, 0, 0),
    },
];

fn setup(theme: &Theme) -> Setup {
    Setup {
        width: 80,
        height: 40,
        colors: Colors::new(ColorLevel::TrueColor, Some(theme.background))
            .with_foreground(Some(theme.foreground)),
        environments: environments(Arc::new(MemoryEnvironment::new(EnvFacts {
            cwd: EnvPath::new("/Users/dev/project"),
            platform: "darwin".into(),
            os_version: "x".into(),
            shell: "zsh".into(),
            is_git_repo: false,
            home: Some(EnvPath::new("/Users/dev")),
            uid: None,
        }))),
        ..Setup::default()
    }
}

fn frame(ui: &Ui) -> Shot {
    ui.frame()
}

/// 第一处 `needle` 所在的（列，行）。
fn find(buffer: &Shot, needle: &str) -> Option<(u16, u16)> {
    buffer.find(needle)
}

/// 整行铺板的底色。
fn assert_panel(buffer: &Shot, y: u16, panel: Color) {
    for x in buffer.area.left()..buffer.area.right() {
        let cell = &buffer[(x, y)];
        assert_eq!(cell.bg, panel, "column {x} of row {y}: {cell:?}");
    }
}

/// `number`（如 `1  Yes`）所在的行是焦点行：序号与名字是 brand、不加粗，行铺板的底色。
fn assert_focused(buffer: &Shot, number: &str, inks: Inks) {
    let (x, y) = find(buffer, number).unwrap_or_else(|| panic!("no {number}"));
    assert_eq!(x, 3, "the number starts at column 3");
    assert_panel(buffer, y, inks.panel);
    assert_eq!(buffer[(x, y)].fg, inks.brand, "the number");
    let name = &buffer[(x + 3, y)];
    assert_eq!(name.fg, inks.brand, "{name:?}");
    assert!(!name.has(attr::BOLD), "{name:?}");
}

/// 没聚焦的行：序号 fg3，名字 fg2。
fn assert_plain(buffer: &Shot, number: &str, inks: Inks) {
    let (x, y) = find(buffer, number).unwrap_or_else(|| panic!("no {number}"));
    assert_panel(buffer, y, inks.panel);
    assert_eq!(buffer[(x, y)].fg, inks.fg3);
    assert_eq!(buffer[(x + 3, y)].fg, inks.fg2);
}

/// 等到最近一帧满足条件（焦点移动只改颜色，画面的文字不变）；等待有上限，只为让失败的测试结束。
async fn until(ui: &Ui, what: &str, done: impl Fn(&Shot) -> bool) {
    let wait = async {
        while !done(&ui.frame()) {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    };
    if tokio::time::timeout(std::time::Duration::from_secs(10), wait)
        .await
        .is_err()
    {
        panic!("timed out waiting for {what}; screen:\n{}", ui.screen());
    }
}

/// `number` 所在的行聚焦了（序号是 brand）。
fn focused(buffer: &Shot, number: &str, brand: Color) -> bool {
    buffer.find(number).is_some_and(|at| buffer[at].fg == brand)
}

fn touching() -> Arc<GatedModel> {
    GatedModel::scripted(vec![
        tool_use_response(
            "toolu_1",
            "Bash",
            &json!({"command": "touch made.txt", "description": "create a file"}),
        ),
        text_response(&["done"]),
    ])
}

#[tokio::test(flavor = "multi_thread")]
async fn the_permission_ask_has_no_bar_and_typing_chooses_deny() {
    for theme in &THEMES {
        let inks = inks(theme);
        let model = touching();
        with_ui(model.clone(), setup(theme), |mut ui| async move {
            ui.submit("make a file");
            ui.wait_for("Run this command?").await;
            ui.settle().await;
            let buffer = frame(&ui);
            assert_focused(&buffer, "1  Allow once", inks);
            assert_plain(&buffer, "2  Full access", inks);
            assert_plain(&buffer, "3  Deny", inks);
            ui.key(KeyCode::Down);
            until(&ui, "the focus on Full access", |b| {
                focused(b, "2  Full access", inks.brand)
            })
            .await;
            let buffer = frame(&ui);
            assert_focused(&buffer, "2  Full access", inks);
            assert_plain(&buffer, "1  Allow once", inks);
            // 打字：字进输入框，Deny 选中，槽说 ⏎ 做什么。
            ui.type_text("use make");
            ui.wait_for("› use make").await;
            ui.wait_for("⏎ deny and send").await;
            // 光标格在输入框的字后面（输入框的光标，不是板上的输入行）。
            let buffer = frame(&ui);
            let (x, y) = find(&buffer, "use make").unwrap();
            assert_eq!(x, 3);
            assert_eq!(ui.cursor(), Some((x + 8, y)));
            until(&ui, "the focus on Deny", |b| {
                focused(b, "3  Deny", inks.brand)
            })
            .await;
            assert_plain(&frame(&ui), "2  Full access", inks);
            ui.key(KeyCode::Enter);
            ui.wait_for("done").await;
        })
        .await;
        let second = request_text(&model.requests()[1]);
        assert!(second.contains("use make"), "{second}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_plan_approval_has_no_bar_and_typing_keeps_planning() {
    for theme in &THEMES {
        let inks = inks(theme);
        let model = GatedModel::scripted(vec![
            tool_use_response("toolu_1", "EnterPlanMode", &json!({})),
            tool_use_response("toolu_2", "ExitPlanMode", &json!({})),
            text_response(&["revising"]),
        ]);
        with_ui(model.clone(), setup(theme), |mut ui| async move {
            ui.submit("plan first");
            let screen = ui.wait_for("Start on this plan?").await;
            assert!(screen.contains("Keep planning"), "{screen}");
            ui.settle().await;
            let buffer = frame(&ui);
            assert_focused(&buffer, "1  Approve", inks);
            assert_plain(&buffer, "2  Keep planning", inks);
            // Keep planning 要有字：选中它按 ⏎ 不起作用；打了字之后 ⏎ 带着字发出。
            ui.key(KeyCode::Down);
            until(&ui, "the focus on keep planning", |b| {
                focused(b, "2  Keep planning", inks.brand)
            })
            .await;
            assert_plain(&frame(&ui), "1  Approve", inks);
            ui.key(KeyCode::Up);
            until(&ui, "the focus on approve", |b| {
                focused(b, "1  Approve", inks.brand)
            })
            .await;
            ui.type_text("smaller steps");
            ui.wait_for("› smaller steps").await;
            ui.wait_for("⏎ keep planning").await;
            until(&ui, "the focus on keep planning", |b| {
                focused(b, "2  Keep planning", inks.brand)
            })
            .await;
            ui.key(KeyCode::Enter);
            ui.wait_for("revising").await;
        })
        .await;
        let last = request_text(&model.requests()[2]);
        assert!(last.contains("smaller steps"), "{last}");
    }
}
