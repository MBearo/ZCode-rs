//! ← 项目页（修复线 F29，docs/specs/rust-cli/tui-panels.md §2）：所有项目的会话按工作目录分组（当前项目在前）、
//! 筛选、`/` 搜索、`r` 改名、`n` 在别的项目新建、enter 切到别的项目的会话（信任检查），esc 关闭。
use std::sync::Arc;

use crossterm::event::KeyCode;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::store::SessionStore;
use rust_cli_kernel_api::testing::{MemoryStore, text_response};

use super::sessions_seed::{Seed, Trust, seed, services};
use super::support::{GatedModel, Setup, with_ui};

fn idle(screen: &str, reply: &str) -> bool {
    screen.contains(reply) && !screen.contains("esc to interrupt")
}

const SEEDS: [Seed; 4] = [
    Seed {
        id: "sess_here",
        root: "/Users/dev/project",
        title: "Old here",
        prompt: "older question",
        reply: "older answer",
        created: 1_000_000,
        updated: 1_000_000,
    },
    Seed {
        id: "sess_app",
        root: "/Users/dev/work/app",
        title: "Fix login",
        prompt: "fix the login bug",
        reply: "login fixed",
        created: 2_000_000,
        updated: 4_000_000,
    },
    Seed {
        id: "sess_lib_a",
        root: "/Users/dev/work/lib",
        title: "Bump version",
        prompt: "bump the version",
        reply: "bumped",
        created: 3_000_000,
        updated: 2_000_000,
    },
    Seed {
        id: "sess_lib_b",
        root: "/Users/dev/work/lib",
        title: "Release notes",
        prompt: "write the notes",
        reply: "notes written",
        created: 4_000_000,
        updated: 3_000_000,
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
        .find(|line| line.starts_with("› "))
        .unwrap_or_default()
        .to_owned()
}

/// 按 ↓ 直到焦点行含 `text`。
async fn focus(ui: &mut super::support::Ui, text: &str) {
    for _ in 0..8 {
        if focused(&ui.screen()).contains(text) {
            return;
        }
        let before = focused(&ui.screen());
        ui.key(KeyCode::Down);
        ui.wait("the focus to move", |s| focused(s) != before).await;
    }
    assert!(focused(&ui.screen()).contains(text), "{}", ui.screen());
}

#[tokio::test(flavor = "multi_thread")]
async fn left_opens_the_projects_grouped_with_the_current_one_first() {
    let store = Arc::new(MemoryStore::default());
    seed(&store, &SEEDS).await;
    let setup = Setup {
        width: 120,
        height: 30,
        store,
        ..Setup::default()
    };
    let model = GatedModel::scripted(vec![text_response(&["first reply"])]);
    with_ui(model, setup, |mut ui| async move {
        let screen = ui.wait_for("← for projects · ? for shortcuts").await;
        assert!(!screen.contains("for agents"), "{screen}");
        ui.submit("first question");
        ui.wait("the turn", |s| idle(s, "first reply")).await;
        ui.key(KeyCode::Left);
        let screen = ui.wait_for("~/work/lib  2").await;
        assert!(screen.contains("  Projects"), "{screen}");
        // 本进程打开着的当前会话是 Ready，只在库里的是 Inactive。
        assert!(
            screen.contains("All 5   Needs you 0   Working 0   Ready 1   Inactive 4"),
            "{screen}"
        );
        // 当前项目的组在最前，其余按路径；组内按更新时间从新到旧。
        assert!(
            row(&screen, "~/project  2") < row(&screen, "~/work/app  1"),
            "{screen}"
        );
        assert!(
            row(&screen, "Old here") < row(&screen, "first question (current)"),
            "{screen}"
        );
        assert!(
            row(&screen, "~/work/app  1") < row(&screen, "~/work/lib  2"),
            "{screen}"
        );
        assert!(
            row(&screen, "Release notes") < row(&screen, "Bump version"),
            "{screen}"
        );
        // 焦点在当前会话；右侧详情：状态、项目路径、首条输入与最近一条回复（宽度不小于 90 列）。
        assert!(
            focused(&screen).contains("first question (current)"),
            "{screen}"
        );
        assert!(focused(&screen).contains("Ready"), "{screen}");
        assert!(screen.contains("│ Session details"), "{screen}");
        assert!(screen.contains("│ ~/project"), "{screen}");
        assert!(screen.contains("│ first reply"), "{screen}");
        // 当前会话按 enter：只关闭页面并提示（Codex）。
        ui.key(KeyCode::Enter);
        ui.wait_for("Already viewing first question.").await;
        ui.key(KeyCode::Left);
        ui.wait_for("~/work/lib  2").await;
        assert!(
            screen.contains(
                "↑/↓ move · enter open · n new · / search · r rename · o resume · esc back"
            ),
            "{screen}"
        );
        // `/` 搜索：标题、首条输入、路径的子串，不分大小写。
        ui.key(KeyCode::Char('/'));
        ui.wait_for("Search ›").await;
        ui.type_text("NOTES");
        let screen = ui.wait_for("Search › NOTES").await;
        assert!(screen.contains("~/work/lib  1 of 2"), "{screen}");
        assert!(screen.contains("Release notes"), "{screen}");
        assert!(!screen.contains("Bump version"), "{screen}");
        assert!(!screen.contains("Fix login"), "{screen}");
        assert!(screen.contains("enter open · esc cancel"), "{screen}");
        ui.key(KeyCode::Esc);
        let screen = ui
            .wait("the search to close", |s| !s.contains("Search ›"))
            .await;
        assert!(screen.contains("Bump version"), "{screen}");
        // tab 切到“需要你”：没有会话。
        ui.key(KeyCode::Tab);
        ui.wait_for("No sessions").await;
        ui.key(KeyCode::Esc);
        ui.wait("the page to close", |s| !s.contains("tab/⇧tab filter"))
            .await;
    })
    .await;
}

/// enter 打开别的项目的会话：那个目录未信任时先问；拒绝回到项目页，同意后切过去（会话自己的工作目录）。
#[tokio::test(flavor = "multi_thread")]
async fn enter_switches_to_a_session_of_another_project_after_the_trust_check() {
    let store = Arc::new(MemoryStore::default());
    seed(&store, &SEEDS).await;
    let trust = Arc::new(Trust {
        untrusted: vec!["/Users/dev/work/lib"],
        ..Trust::default()
    });
    let setup = Setup {
        width: 100,
        height: 30,
        store,
        services: services(&trust),
        ..Setup::default()
    };
    let model = GatedModel::scripted(vec![text_response(&["continued in lib"])]);
    let checked = trust.clone();
    with_ui(model.clone(), setup, |mut ui| async move {
        ui.wait_for("← for projects").await;
        ui.key(KeyCode::Left);
        ui.wait_for("~/work/lib  2").await;
        focus(&mut ui, "Release notes").await;
        ui.key(KeyCode::Enter);
        ui.wait_for("Accessing workspace").await;
        // `No, exit` 只取消这次切换（tui-panels.md D13）。
        ui.key(KeyCode::Enter);
        let screen = ui.wait_for("~/work/lib  2").await;
        assert!(focused(&screen).contains("Release notes"), "{screen}");
        ui.key(KeyCode::Enter);
        ui.wait_for("Accessing workspace").await;
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Enter);
        let screen = ui
            .wait("the lib session", |s| {
                s.contains("notes written") && !s.contains("tab/⇧tab filter")
            })
            .await;
        assert!(screen.contains("write the notes"), "{screen}");
        assert_eq!(*checked.accepted.lock().unwrap(), ["/Users/dev/work/lib"]);
        ui.submit("go on");
        ui.wait("the reply", |s| idle(s, "continued in lib")).await;
    })
    .await;
    // 续写在原会话上：请求里是那个会话的历史加新输入。
    let text = super::support::request_text(&model.requests()[0]);
    assert!(
        text.contains("write the notes") && text.contains("go on"),
        "{text}"
    );
}

/// `n` 在选中行的项目新建会话；`r` 改名写进库（不是当前会话：直接写存储）。
#[tokio::test(flavor = "multi_thread")]
async fn n_starts_a_session_in_another_project_and_r_renames() {
    let store = Arc::new(MemoryStore::default());
    seed(&store, &SEEDS).await;
    let setup = Setup {
        width: 100,
        height: 30,
        store: store.clone(),
        ..Setup::default()
    };
    let model = GatedModel::scripted(vec![text_response(&["hello from app"])]);
    let session = with_ui(model, setup, |mut ui| async move {
        ui.wait_for("← for projects").await;
        ui.key(KeyCode::Left);
        ui.wait_for("~/work/app  1").await;
        focus(&mut ui, "Fix login").await;
        ui.key(KeyCode::Char('r'));
        ui.wait_for("Rename › Fix login").await;
        for _ in 0.."login".len() {
            ui.key(KeyCode::Backspace);
        }
        ui.type_text("sign-in");
        ui.wait_for("Rename › Fix sign-in").await;
        ui.key(KeyCode::Enter);
        let screen = ui
            .wait("the renamed row", |s| {
                s.contains("Fix sign-in") && !s.contains("Rename ›")
            })
            .await;
        assert!(focused(&screen).contains("Fix sign-in"), "{screen}");
        ui.key(KeyCode::Char('n'));
        ui.wait_for("Started a new session in ~/work/app.").await;
        ui.submit("hi there");
        ui.wait("the reply", |s| idle(s, "hello from app")).await;
    })
    .await;
    assert_eq!(
        store.title(&SessionId::new("sess_app")).as_deref(),
        Some("Fix sign-in")
    );
    let id = SessionId::new(session.expect("a new session"));
    let loaded = store.load(&id).await.unwrap();
    assert_eq!(loaded.meta.workspace.root.as_str(), "/Users/dev/work/app");
}

#[tokio::test(flavor = "multi_thread")]
async fn chinese_labels() {
    let store = Arc::new(MemoryStore::default());
    seed(&store, &SEEDS[..1]).await;
    let setup = Setup {
        lang: "zh_CN.UTF-8",
        store,
        ..Setup::default()
    };
    with_ui(
        GatedModel::scripted(Vec::new()),
        setup,
        |mut ui| async move {
            ui.wait_for("← 查看项目").await;
            ui.key(KeyCode::Left);
            let screen = ui.wait_for("~/project  1").await;
            assert!(screen.contains("  项目"), "{screen}");
            assert!(screen.contains("全部 1"), "{screen}");
            assert!(
                screen.contains("{n} 新建".replace(['{', '}'], "").as_str()),
                "{screen}"
            );
            ui.key(KeyCode::Char('q'));
            ui.wait("the page to close", |s| !s.contains("tab/⇧tab 筛选"))
                .await;
        },
    )
    .await;
}
