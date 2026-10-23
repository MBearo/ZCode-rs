//! 修复线 F25（docs/specs/rust-cli/tui-select.md §4）：滚轮按最上层的可滚动视图分发，没有时滚对话区。
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers};
use rust_cli_kernel_api::env::{EnvFacts, EnvPath};
use rust_cli_kernel_api::testing::{MemoryEnvironment, text_response};

use super::support::{GatedModel, Setup, Ui, with_ui};

/// 屏幕上第 `row` 行的文字。
fn line(screen: &str, row: usize) -> String {
    screen.lines().nth(row).unwrap_or_default().to_owned()
}

/// 滚轮向下滚几格后，`row` 行的文字变了（页面往下滚了），再滚回去又回到原样。
async fn scrolls(ui: &mut Ui, row: usize, what: &str) {
    let before = ui.screen();
    let top = line(&before, row);
    ui.wheel(true, 3, 10, 5);
    let after = ui
        .wait(&format!("{what} to scroll down"), |s| line(s, row) != top)
        .await;
    assert_ne!(line(&after, row), top, "{after}");
    ui.wheel(false, 20, 10, 5);
    ui.wait(&format!("{what} to scroll back"), |s| line(s, row) == top)
        .await;
}

fn short() -> Setup {
    Setup {
        height: 12,
        ..Setup::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_help_page_scrolls() {
    with_ui(
        GatedModel::scripted(Vec::new()),
        short(),
        |mut ui| async move {
            ui.wait_for("Ask before changes").await;
            ui.submit("/help");
            ui.wait_for("/clear").await;
            // 缺陷（F25）：原来独立页面没有滚轮分支，滚轮滚的是被页面盖住的对话区。
            scrolls(&mut ui, 3, "the help page").await;
        },
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_status_page_scrolls() {
    let setup = Setup {
        height: 7,
        ..Setup::default()
    };
    with_ui(
        GatedModel::scripted(Vec::new()),
        setup,
        |mut ui| async move {
            ui.wait_for("Ask before changes").await;
            ui.submit("/status");
            ui.wait_for("Version:").await;
            scrolls(&mut ui, 2, "the status page").await;
        },
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_context_page_scrolls() {
    let setup = Setup {
        height: 8,
        ..Setup::default()
    };
    with_ui(
        GatedModel::scripted(Vec::new()),
        setup,
        |mut ui| async move {
            ui.wait_for("Ask before changes").await;
            ui.submit("/context");
            ui.wait_for("Context Usage").await;
            scrolls(&mut ui, 3, "the context page").await;
        },
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_skills_page_scrolls() {
    let mut env = MemoryEnvironment::new(EnvFacts {
        cwd: EnvPath::new("/Users/dev/project"),
        platform: "darwin".into(),
        os_version: "x".into(),
        shell: "zsh".into(),
        is_git_repo: false,
        additional_directories: Vec::new(),
        home: Some(EnvPath::new("/Users/dev")),
    });
    for index in 0..30 {
        env = env.with_file(
            &format!("/Users/dev/project/.zcode/skills/skill-{index:02}/SKILL.md"),
            &format!("---\ndescription: Skill number {index}\n---\nBody.\n"),
        );
    }
    let setup = Setup {
        env: Arc::new(env),
        ..short()
    };
    with_ui(
        GatedModel::scripted(Vec::new()),
        setup,
        |mut ui| async move {
            ui.wait_for("Ask before changes").await;
            ui.submit("/skills");
            ui.wait_for("skill-00").await;
            scrolls(&mut ui, 3, "the skills page").await;
        },
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_usage_page_scrolls() {
    let setup = Setup {
        height: 8,
        ..Setup::default()
    };
    with_ui(
        GatedModel::scripted(Vec::new()),
        setup,
        |mut ui| async move {
            ui.wait_for("Ask before changes").await;
            ui.submit("/usage");
            ui.wait_for("Usage").await;
            scrolls(&mut ui, 3, "the usage page").await;
        },
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_detail_view_and_the_transcript_scroll() {
    let reply: String = (0..40).map(|n| format!("reply line {n:02}\n")).collect();
    let model = GatedModel::scripted(vec![text_response(&[reply.as_str(), "done"])]);
    with_ui(model, short(), |mut ui| async move {
        ui.submit("hi");
        ui.wait("the reply", |s| {
            s.contains("done") && !s.contains("esc to interrupt")
        })
        .await;
        // 对话区：向上滚之后最后一行不在屏幕上，滚回来又出现。
        ui.wheel(false, 6, 10, 3);
        ui.wait("the transcript to scroll up", |s| {
            !s.contains("reply line 39")
        })
        .await;
        ui.wheel(true, 20, 10, 3);
        ui.wait_for("reply line 39").await;
        ui.press(KeyCode::Char('o'), KeyModifiers::CONTROL);
        ui.wait_for("Showing detailed transcript").await;
        ui.wheel(false, 6, 10, 3);
        ui.wait("the detail view to scroll up", |s| {
            s.contains("Showing detailed transcript") && !s.contains("reply line 39")
        })
        .await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_picker_moves_its_selection_and_leaves_the_transcript() {
    let reply: String = (0..40).map(|n| format!("reply line {n:02}\n")).collect();
    let model = GatedModel::scripted(vec![text_response(&[reply.as_str(), "done"])]);
    let setup = Setup {
        height: 24,
        ..Setup::default()
    };
    with_ui(model, setup, |mut ui| async move {
        ui.submit("hi");
        ui.wait("the reply", |s| {
            s.contains("done") && !s.contains("esc to interrupt")
        })
        .await;
        ui.submit("/model");
        ui.wait_for("› ").await;
        let before = ui.wait_for("other-model").await;
        assert!(!before.contains("› other-model"), "{before}");
        ui.wheel(true, 1, 10, 20);
        let after = ui.wait_for("› other-model").await;
        // 列表拿走了滚轮：对话区停在底部。
        assert!(
            after.contains("reply line 39") || after.contains("done"),
            "{after}"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_permissions_list_moves_with_the_wheel() {
    with_ui(
        GatedModel::scripted(Vec::new()),
        Setup::default(),
        |mut ui| async move {
            ui.wait_for("Ask before changes").await;
            ui.submit("/permissions");
            ui.wait_for("› Add a new rule…").await;
            // 加一条规则，列表里就有两项。
            ui.key(KeyCode::Enter);
            ui.wait_for("Add allow permission rule").await;
            ui.type_text("Bash(ls *)");
            ui.key(KeyCode::Enter);
            ui.wait_for("Where should this rule be saved?").await;
            ui.key(KeyCode::Enter);
            // 等保存的回执（提示从 `Saving…` 回到列表的提示）。
            ui.wait("the saved rule", |s| {
                s.contains("    Bash(ls *)") && s.contains("to navigate")
            })
            .await;
            ui.wheel(true, 1, 10, 10);
            // 缺陷（F25）：原来 `/permissions` 没有滚轮分支，选中项不动。
            ui.wait_for("› Bash(ls *)").await;
            ui.wheel(false, 1, 10, 10);
            ui.wait_for("› Add a new rule…").await;
        },
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_sessions_panel_and_the_resume_list_move_with_the_wheel() {
    let model = GatedModel::scripted(vec![
        text_response(&["first reply"]),
        text_response(&["second reply"]),
    ]);
    with_ui(model, Setup::default(), |mut ui| async move {
        let idle = |s: &str, reply: &str| s.contains(reply) && !s.contains("esc to interrupt");
        ui.submit("first question");
        ui.wait("the first turn", |s| idle(s, "first reply")).await;
        ui.submit("/clear");
        ui.wait_for("Started a new session.").await;
        ui.submit("second question");
        ui.wait("the second turn", |s| idle(s, "second reply"))
            .await;
        // ← 项目页（修复线 F29，原来是会话面板）：滚轮移动选中项（原来另外还滚了被盖住的对话区）。
        ui.key(KeyCode::Left);
        ui.wait_for(" Ready 1   Inactive 1 ").await;
        let current = |s: &str| {
            s.lines()
                .any(|l| l.starts_with("› ") && l.contains("(current)"))
        };
        // 选中的是第一行时往下滚，否则往上滚。
        let screen = ui.screen();
        let rows: Vec<&str> = screen.lines().filter(|l| l.contains(" question")).collect();
        let down = rows.first().is_some_and(|l| l.starts_with("› "));
        let was = current(&screen);
        ui.wheel(down, 1, 10, 5);
        ui.wait("the other session", |s| current(s) != was).await;
        ui.key(KeyCode::Esc);
        ui.wait("the panel to close", |s| !s.contains("tab/⇧tab filter"))
            .await;
        // `/resume` 页（修复线 F29）：滚轮移动焦点，到头停住。
        ui.submit("/resume");
        // 缺陷（修复线 F31，测试的写法）：原来只等页面框架 `Type to search`，框架先于会话列表画出，滚轮几乎总落在
        // 加载中（插桩：不加负载 10 次 10 次、负载下 200 次 192 次），焦点从“没有”变成第一行，这一段没有测到滚轮。
        // 现在等两个会话的行都出现。
        let list = ui
            .wait("the resume list", |s| {
                s.lines().filter(|line| line.contains(" question")).count() == 2
            })
            .await;
        let focused = |s: &str| {
            s.lines()
                .find(|l| l.starts_with("  › "))
                .map(str::to_owned)
                .unwrap_or_default()
        };
        let before = focused(&list);
        // 选中的是第一项时往下滚，否则往上滚（同一时刻的两个会话，先后不定）。
        let first = list
            .lines()
            .find(|l| l.contains(" question"))
            .unwrap_or_default();
        ui.wheel(first.starts_with("  › "), 1, 10, 8);
        ui.wait("the resume list to move", |s| focused(s) != before)
            .await;
    })
    .await;
}
