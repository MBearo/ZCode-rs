//! `/resume` 独立页面（修复线 F29，docs/specs/rust-cli/tui-manage.md §10）：筛选当前目录 / 全部、排序、直接打字搜索、
//! ⌃e 预览、⌃t 全屏对话、⌃r 改名、⌃d 删除确认、当前会话的限制、别的目录的会话先选目录。
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::testing::{MemoryStore, text_response};

use super::sessions_seed::{Seed, seed};
use super::support::{GatedModel, Setup, Ui, with_ui};

fn idle(screen: &str, reply: &str) -> bool {
    screen.contains(reply) && !screen.contains("esc to interrupt")
}

/// 当前目录两个会话、别的目录一个；创建与活动的先后相反（两种排序的顺序不同）。
const SEEDS: [Seed; 3] = [
    Seed {
        id: "sess_alpha",
        root: "/Users/dev/project",
        title: "Alpha task",
        prompt: "alpha prompt",
        reply: "alpha reply",
        created: 1_000_000,
        updated: 3_000_000,
    },
    Seed {
        id: "sess_beta",
        root: "/Users/dev/project",
        title: "",
        prompt: "beta prompt words",
        reply: "beta reply",
        created: 2_000_000,
        updated: 2_000_000,
    },
    Seed {
        id: "sess_other",
        root: "/Users/dev/other",
        title: "Other place",
        prompt: "other prompt",
        reply: "other reply",
        created: 3_000_000,
        updated: 1_000_000,
    },
];

fn row(screen: &str, text: &str) -> usize {
    screen
        .lines()
        .position(|line| line.contains(text))
        .unwrap_or_else(|| panic!("no {text:?} on the screen:\n{screen}"))
}

fn focused(screen: &str) -> String {
    screen
        .lines()
        .find(|line| line.trim_start().starts_with("› ") || line.trim_start().starts_with("⌄ "))
        .unwrap_or_default()
        .to_owned()
}

fn ctrl(ui: &Ui, c: char) {
    ui.press(KeyCode::Char(c), KeyModifiers::CONTROL);
}

async fn setup(width: u16) -> (Arc<MemoryStore>, Setup) {
    let store = Arc::new(MemoryStore::default());
    seed(&store, &SEEDS).await;
    let setup = Setup {
        width,
        height: 30,
        store: store.clone(),
        ..Setup::default()
    };
    (store, setup)
}

#[tokio::test(flavor = "multi_thread")]
async fn filter_sort_and_search() {
    let (_, setup) = setup(120).await;
    with_ui(GatedModel::scripted(Vec::new()), setup, |mut ui| async move {
        ui.submit("/resume");
        let screen = ui.wait_for("Alpha task").await;
        assert!(screen.contains(" Resume a previous session"), "{screen}");
        assert!(screen.contains("Filter:  Cwd  All    Sort:  Updated  Created"), "{screen}");
        assert!(screen.contains("Type to search"), "{screen}");
        // Filter=Cwd：只有当前目录；按更新时间。没有名字的用首条输入。
        assert!(!screen.contains("Other place"), "{screen}");
        assert!(row(&screen, "Alpha task") < row(&screen, "beta prompt words"), "{screen}");
        assert!(focused(&screen).contains("Alpha task"), "{screen}");
        assert!(screen.contains(" 1 / 2 · 100% "), "{screen}");
        assert!(
            screen.contains("enter resume   ⌃r rename   ⌃d delete   esc exit   ⌃c exit   tab focus sort/filter   ←/→ change option"),
            "{screen}"
        );
        assert!(
            screen.contains("⌃o comfortable view   ⌃t transcript   ⌃e expand   ↑/↓ browse"),
            "{screen}"
        );
        // → 切到 All：别的目录的会话与它的路径。
        ui.key(KeyCode::Right);
        let screen = ui.wait_for("Other place").await;
        assert!(screen.contains("⌁ ~/other"), "{screen}");
        assert!(screen.contains(" 1 / 3 · 100% "), "{screen}");
        // tab 到 Sort、→ 切到 Created：创建时间从新到旧。
        ui.key(KeyCode::Tab);
        ui.key(KeyCode::Right);
        let screen = ui
            .wait("the created order", |s| {
                s.contains("Other place")
                    && s.contains("Alpha task")
                    && row(s, "Other place") < row(s, "Alpha task")
            })
            .await;
        assert!(row(&screen, "beta prompt words") < row(&screen, "Alpha task"), "{screen}");
        // 直接打字就搜（含 j、k 与空格）：标题、首条输入、路径。
        ui.type_text("beta pr");
        let screen = ui.wait_for("Search: beta pr").await;
        assert!(screen.contains("beta prompt words"), "{screen}");
        assert!(!screen.contains("Alpha task"), "{screen}");
        assert!(screen.contains("esc clear search"), "{screen}");
        ui.key(KeyCode::Esc);
        let screen = ui.wait_for("Type to search").await;
        assert!(screen.contains("Alpha task"), "{screen}");
        ui.type_text("zzz");
        ui.wait_for("No results for your search").await;
        ui.key(KeyCode::Esc);
        ui.key(KeyCode::Esc);
        ui.wait("the page to close", |s| !s.contains("Type to search")).await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn expand_previews_and_transcript() {
    let (_, setup) = setup(100).await;
    with_ui(
        GatedModel::scripted(Vec::new()),
        setup,
        |mut ui| async move {
            ui.submit("/resume");
            ui.wait_for("Alpha task").await;
            ctrl(&ui, 'e');
            let screen = ui.wait_for("│ alpha prompt").await;
            assert!(focused(&screen).starts_with("  ⌄ "), "{screen}");
            assert!(
                screen.contains("│ Session:    Alpha task (sess_alpha)"),
                "{screen}"
            );
            assert!(screen.contains("│ Directory:  ~/project"), "{screen}");
            assert!(screen.contains("│ Branch:     ⎇ no branch"), "{screen}");
            assert!(screen.contains("│ Conversation:"), "{screen}");
            assert!(screen.contains("└ alpha reply"), "{screen}");
            ctrl(&ui, 'e');
            ui.wait("the preview to close", |s| !s.contains("Conversation:"))
                .await;
            ctrl(&ui, 't');
            let screen = ui.wait_for("q close").await;
            assert!(screen.contains("  Alpha task"), "{screen}");
            assert!(screen.contains("alpha prompt"), "{screen}");
            assert!(screen.contains("alpha reply"), "{screen}");
            ui.key(KeyCode::Char('q'));
            let screen = ui.wait_for("Type to search").await;
            assert!(screen.contains("Resume a previous session"), "{screen}");
            // ⌃o 切到 Comfortable：第二行写日期与分支。
            ctrl(&ui, 'o');
            let screen = ui.wait_for("⎇ no branch").await;
            assert!(screen.contains("⌃o dense "), "{screen}");
        },
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn rename_and_delete_with_confirmation() {
    let (store, setup) = setup(100).await;
    let model = GatedModel::scripted(vec![text_response(&["current reply"])]);
    with_ui(model, setup, |mut ui| async move {
        ui.submit("current question");
        ui.wait("the turn", |s| idle(s, "current reply")).await;
        ui.submit("/resume");
        ui.wait_for("Alpha task").await;
        // 当前会话（活动时间最早，排在最后）不能删，也不能“恢复”到自己。
        ui.key(KeyCode::End);
        ui.wait("the current session", |s| {
            focused(s).contains("current question")
        })
        .await;
        ctrl(&ui, 'd');
        ui.wait_for("Use /clear to leave the current session before deleting it.")
            .await;
        // ⌃r 改名。
        ui.key(KeyCode::Home);
        ui.wait("the first row", |s| focused(s).contains("Alpha task"))
            .await;
        ctrl(&ui, 'r');
        ui.wait_for("Rename › Alpha task").await;
        ui.type_text(" v2");
        ui.key(KeyCode::Enter);
        ui.wait("the renamed row", |s| {
            s.contains("Alpha task v2") && !s.contains("Rename ›")
        })
        .await;
        // ⌃d：确认框，esc 取消；再来一次选删除。
        ctrl(&ui, 'd');
        let screen = ui.wait_for("Permanently delete “Alpha task v2”?").await;
        assert!(screen.contains("This cannot be undone."), "{screen}");
        assert!(screen.contains("› Cancel"), "{screen}");
        assert!(screen.contains("enter select · esc cancel"), "{screen}");
        ui.key(KeyCode::Esc);
        ui.wait("the confirmation to close", |s| {
            !s.contains("Permanently delete")
        })
        .await;
        ctrl(&ui, 'd');
        ui.wait_for("Permanently delete “Alpha task v2”?").await;
        ui.key(KeyCode::Char('2'));
        ui.wait_for("› Permanently delete session").await;
        ui.key(KeyCode::Enter);
        let screen = ui
            .wait("the row to go", |s| !s.contains("Alpha task v2"))
            .await;
        assert!(screen.contains("beta prompt words"), "{screen}");
        // enter 选当前会话：提示已在看。
        ui.key(KeyCode::End);
        ui.wait("the current session", |s| {
            focused(s).contains("current question")
        })
        .await;
        ui.key(KeyCode::Enter);
        ui.wait_for("Already viewing current question.").await;
    })
    .await;
    let alpha = SessionId::new("sess_alpha");
    assert_eq!(store.title(&alpha).as_deref(), Some("Alpha task v2"));
    assert!(store.is_deleted(&alpha));
}

/// 别的目录的会话：先选目录。用会话目录（enter 或 esc）：切过去，工作区是会话的目录；用当前目录：打开后经 `/cd` 换到
/// 当前目录（只对这次打开有效，tui-manage.md §10.5）。
async fn resume_other(current: bool) -> String {
    let (_, mut setup) = setup(100).await;
    setup.env = super::sessions_seed::env();
    let model = GatedModel::scripted(Vec::new());
    let (sender, receiver) = std::sync::mpsc::channel();
    with_ui(model, setup, |mut ui| async move {
        ui.submit("/resume");
        ui.wait_for("Alpha task").await;
        ui.key(KeyCode::Right);
        ui.wait_for("Other place").await;
        ui.type_text("other");
        ui.wait("only the other session", |s| {
            focused(s).contains("Other place")
        })
        .await;
        ui.key(KeyCode::Enter);
        let screen = ui.wait_for("Working directory · resume").await;
        assert!(
            screen.contains("› 1. Use session directory (~/other)"),
            "{screen}"
        );
        assert!(
            screen.contains("2. Use current directory (~/project)"),
            "{screen}"
        );
        assert!(
            screen.contains("enter continue · esc use session · ⌃c exit"),
            "{screen}"
        );
        if current {
            ui.key(KeyCode::Down);
            ui.wait("the second choice", |s| {
                s.contains("› 2. Use current directory")
            })
            .await;
            ui.key(KeyCode::Enter);
            ui.wait_for("Moved to ~/project").await;
        } else {
            ui.key(KeyCode::Esc);
        }
        ui.wait("the other session", |s| {
            s.contains("other reply") && !s.contains("Working directory")
        })
        .await;
        ui.submit("/export chat");
        let screen = ui.wait_for("Conversation exported to: ").await;
        sender.send(screen).unwrap();
    })
    .await;
    receiver.recv().unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn another_directory_uses_the_session_directory() {
    let screen = resume_other(false).await;
    assert!(
        screen.contains("Conversation exported to: /Users/dev/other/chat.txt"),
        "{screen}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn another_directory_can_use_the_current_directory() {
    let screen = resume_other(true).await;
    assert!(
        screen.contains("Conversation exported to: /Users/dev/project/chat.txt"),
        "{screen}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn chinese_labels() {
    let (_, mut setup) = setup(120).await;
    setup.lang = "zh_CN.UTF-8";
    with_ui(
        GatedModel::scripted(Vec::new()),
        setup,
        |mut ui| async move {
            ui.submit("/resume");
            let screen = ui.wait_for("Alpha task").await;
            assert!(screen.contains("恢复以前的会话"), "{screen}");
            assert!(screen.contains("键入以搜索"), "{screen}");
            assert!(screen.contains("筛选：  当前目录  全部"), "{screen}");
            ui.key(KeyCode::Esc);
            ui.wait("the page to close", |s| !s.contains("键入以搜索"))
                .await;
        },
    )
    .await;
}
