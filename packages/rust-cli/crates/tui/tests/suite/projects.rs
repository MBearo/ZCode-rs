//! ←：所有文件夹的会话列表（以前的项目页，tui-kit.md §4.3）：最新的在上，细节行写会话的文件夹；⏎ 别的文件夹的会话先问
//! 在哪里打开，再做信任检查。
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers};
use rust_cli_kernel_api::testing::{MemoryStore, text_response};

use super::sessions_seed::{Seed, Trust, seed, services};
use super::support::{GatedModel, Setup, with_ui};

fn idle(screen: &str, reply: &str) -> bool {
    screen.contains(reply) && !screen.contains("esc stop")
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

#[tokio::test(flavor = "multi_thread")]
async fn left_opens_every_folder_newest_first() {
    let store = Arc::new(MemoryStore::default());
    seed(&store, &SEEDS).await;
    let setup = Setup {
        width: 120,
        height: 30,
        store,
        ..Setup::default()
    };
    with_ui(
        GatedModel::scripted(Vec::new()),
        setup,
        |mut ui| async move {
            ui.wait_for("? keys").await;
            ui.key(KeyCode::Left);
            let screen = ui.wait_for("Old here").await;
            assert!(screen.contains("Agent cockpit"), "{screen}");
            assert!(screen.contains("Type to filter"), "{screen}");
            assert!(screen.contains("Resting"), "{screen}");
            let at = |t: &str| {
                screen
                    .lines()
                    .position(|l| l.contains(t))
                    .unwrap_or_else(|| panic!("no {t:?}:\n{screen}"))
            };
            assert!(at("Fix login") < at("Release notes"), "{screen}");
            assert!(at("Release notes") < at("Bump version"), "{screen}");
            assert!(at("Bump version") < at("Old here"), "{screen}");
            // 选中的（最急的、最新的）代理在窗格里：标题写两次，文件夹的路径只在窗格。
            assert_eq!(screen.matches("Fix login").count(), 2, "{screen}");
            assert!(screen.contains("~/work/app"), "{screen}");
            // 点一行选中它，窗格换上它（tui-cockpit.md §3.1）。
            let row = screen.lines().position(|l| l.contains("Old here")).unwrap();
            let row = u16::try_from(row).unwrap();
            let left = crossterm::event::MouseButton::Left;
            ui.mouse(crossterm::event::MouseEventKind::Down(left), 12, row);
            ui.mouse(crossterm::event::MouseEventKind::Up(left), 12, row);
            ui.wait("the pane follows the click", |s| {
                s.matches("Old here").count() == 2
            })
            .await;
            // 打字只进草稿：`/` 不开命令架（它匹配路径，四个都留下）；esc 先清词，再回去。
            ui.type_text("/");
            ui.wait_for("4 of 4 match").await;
            ui.key(KeyCode::Esc);
            ui.wait_for("4 resting").await;
            ui.key(KeyCode::Esc);
            ui.wait("back at the transcript", |s| !s.contains("Agent cockpit"))
                .await;
        },
    )
    .await;
}

/// 别的文件夹的会话：先问在哪里打开，再做信任检查；`No, exit` 只取消这次切换（tui-panels.md D13）。
#[tokio::test(flavor = "multi_thread")]
async fn enter_switches_to_a_session_of_another_folder_after_the_trust_check() {
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
        ui.wait_for("? keys").await;
        ui.key(KeyCode::Left);
        ui.wait_for("Old here").await;
        ui.type_text("notes");
        ui.wait("only the notes", |s| {
            s.contains("Release notes") && !s.contains("Old here")
        })
        .await;
        ui.key(KeyCode::Enter);
        ui.wait_for("Open “Release notes” in which folder?").await;
        ui.key(KeyCode::Enter);
        ui.wait_for("Accessing workspace").await;
        ui.key(KeyCode::Enter);
        ui.wait("the trust ask to close", |s| {
            !s.contains("Accessing workspace") && !s.contains("Opening “")
        })
        .await;
        ui.key(KeyCode::Left);
        ui.wait_for("Release notes").await;
        ui.type_text("notes");
        ui.wait("only the notes", |s| {
            s.contains("Release notes") && !s.contains("Old here")
        })
        .await;
        ui.key(KeyCode::Enter);
        ui.wait_for("in which folder?").await;
        ui.key(KeyCode::Char('1'));
        ui.wait_for("Accessing workspace").await;
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Enter);
        let screen = ui
            .wait("the lib session", |s| {
                s.contains("notes written") && !s.contains("Accessing workspace")
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

/// 应用内的信任框按一次 ^C：最后一行换成 `^C again to exit`，不会过时（与启动前的信任页相同，F138-2 的决定：提示在时
/// 再按一次就生效，显示与行为一致）；过了以前的到期时间再按 ^C，取消这次切换（同 `No, exit`，不退出进程）。
#[tokio::test(flavor = "multi_thread")]
async fn the_in_app_trust_hint_stays_until_the_second_ctrl_c() {
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
    let checked = trust.clone();
    with_ui(
        GatedModel::scripted(Vec::new()),
        setup,
        |mut ui| async move {
            ui.wait_for("? keys").await;
            ui.key(KeyCode::Left);
            ui.wait_for("Old here").await;
            ui.type_text("notes");
            ui.wait("only the notes", |s| {
                s.contains("Release notes") && !s.contains("Old here")
            })
            .await;
            ui.key(KeyCode::Enter);
            ui.wait_for("in which folder?").await;
            ui.key(KeyCode::Char('1'));
            ui.wait_for("Accessing workspace").await;
            ui.press(KeyCode::Char('c'), KeyModifiers::CONTROL);
            ui.wait_for("^C again to exit").await;
            // 等过以前的到期时间（800 ms）：这里断言的是“没有发生”，只能让时间走过去。
            tokio::time::sleep(std::time::Duration::from_millis(1_200)).await;
            let screen = ui.screen();
            assert!(screen.contains("^C again to exit"), "{screen}");
            assert!(screen.contains("Accessing workspace"), "{screen}");
            ui.press(KeyCode::Char('c'), KeyModifiers::CONTROL);
            ui.wait("the switch to be cancelled", |s| {
                !s.contains("Accessing workspace") && !s.contains("Opening “")
            })
            .await;
        },
    )
    .await;
    assert!(checked.accepted.lock().unwrap().is_empty());
}
