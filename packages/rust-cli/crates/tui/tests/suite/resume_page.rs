//! `/resume` 的会话列表（tui-kit.md §4.2「`/resume`」）：板上跟着草稿的一层，草稿里的词搜索；旧界面才有的 ⇥ 所有文件夹、
//! ⌃r 改名、⌃d 删除（编号的回答）、⌃t 读对话（一页，`c` 复制它）、⌃e 按创建时间排、⌃f 按状态筛、现在的会话的限制、别的
//! 文件夹的会话先问在哪里打开（sessions_steps.rs）。
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::testing::{MemoryStore, environments, text_response};

use rust_cli_tui_kit::render::{Color, attr};

use super::sessions_seed::{Seed, seed};
use super::support::{GatedModel, Setup, Shot, Ui, with_ui};

fn idle(screen: &str, reply: &str) -> bool {
    screen.contains(reply) && !screen.contains("esc stop")
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

/// 会话的标题从这一列起：第 3 列、年龄 6 格、空格、记号格、空格（Puffin 的会话行，tui-kit.md §4.2「`/resume` 页」）。
const TITLE_AT: u16 = 12;

/// 选中的会话行（S3 起没有 `›` 指针，看样式）：标题列是选中的墨的那一行。套件的终端没报前景，主题是 plain：选中的墨是
/// brand 加粗，没选中的 fg2 是 dim、没有颜色。工具栏与筛选标签那一行也可能有选中的墨，跳过。← 项目页的行同样的列。
pub(super) fn chosen_row(shot: &Shot) -> String {
    (0..shot.area.height)
        .map(|y| (y, shot.row(y)))
        .find(|(y, text)| {
            let cell = &shot[(TITLE_AT, *y)];
            // 标题前一格是记号后的空格；项目页的筛选标签那一行这里是字（`Needs you`）。
            !text.contains("Filter:")
                && !text.contains("⇧tab")
                && shot[(TITLE_AT - 1, *y)].symbol() == " "
                && cell.has(attr::BOLD)
                && cell.fg != Color::Default
        })
        .map(|(_, text)| text)
        .unwrap_or_default()
}

/// `text` 的第一格带颜色（选中的选项：选中的墨或删除的 err；没选中的 fg2 在 plain 里没有颜色）。
pub(super) fn coloured(shot: &Shot, text: &str) -> bool {
    shot.find(text)
        .is_some_and(|(x, y)| shot[(x, y)].fg != Color::Default)
}

/// 等到最近一帧满足条件（看样式的条件，`Ui::wait` 只给文字）；短间隔轮询，有上限。
pub(super) async fn wait_frame(ui: &Ui, what: &str, done: impl Fn(&Shot) -> bool) -> Shot {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let shot = ui.frame();
        if done(&shot) {
            return shot;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}; screen:\n{}",
            shot.text()
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// 等到选中的会话行含 `text`。
async fn wait_chosen(ui: &Ui, what: &str, text: &str) -> String {
    let shot = wait_frame(ui, what, |shot| chosen_row(shot).contains(text)).await;
    chosen_row(&shot)
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
async fn the_list_follows_the_draft_and_tab_shows_every_folder() {
    let (_, setup) = setup(120).await;
    with_ui(
        GatedModel::scripted(Vec::new()),
        setup,
        |mut ui| async move {
            ui.submit("/resume");
            let screen = ui.wait_for("Alpha task").await;
            // 这个文件夹的会话，最新的在上；没有名字的用首条输入。
            assert!(!screen.contains("Other place"), "{screen}");
            assert!(
                row(&screen, "Alpha task") < row(&screen, "beta prompt words"),
                "{screen}"
            );
            assert!(screen.contains("/resume"), "{screen}");
            // ⇥ 所有文件夹：别的文件夹的会话，细节行先写它的文件夹。
            ui.key(KeyCode::Tab);
            ui.wait_for("Other place").await;
            ui.type_text("other");
            wait_chosen(&ui, "only the other session", "Other place").await;
            let screen = ui.wait_for("~/other").await;
            assert!(!screen.contains("Alpha task"), "{screen}");
            // esc 关上列表，草稿留着。
            ui.key(KeyCode::Esc);
            let screen = ui
                .wait("the list to close", |s| !s.contains("Other place"))
                .await;
            assert!(screen.contains("/resume other"), "{screen}");
        },
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ctrl_t_reads_a_session_as_a_page() {
    let (_, setup) = setup(100).await;
    with_ui(
        GatedModel::scripted(Vec::new()),
        setup,
        |mut ui| async move {
            ui.submit("/resume");
            wait_chosen(&ui, "the first row", "Alpha task").await;
            ctrl(&ui, 't');
            let screen = ui.wait_for("alpha reply").await;
            assert!(screen.contains("   Sessions  ›  Alpha task"), "{screen}");
            assert!(screen.contains("alpha prompt"), "{screen}");
            // `c` 复制这一页的字（tui-kit.md §4.3「页的复制」）。
            ui.key(KeyCode::Char('c'));
            ui.wait_for("Copied the page").await;
            let page = ui.copied().last().cloned().unwrap_or_default();
            // 共同的缩进照 Puffin 去掉（你的消息的 `›` 在第 2 列，面包屑留下两格）。
            assert!(page.starts_with("  Sessions  ›  Alpha task\n"), "{page}");
            assert!(page.contains("› alpha prompt"), "{page}");
            assert!(page.contains("alpha reply"), "{page}");
            ui.key(KeyCode::Esc);
            let screen = ui
                .wait("the list again", |s| !s.contains("Sessions  ›"))
                .await;
            assert!(screen.contains("beta prompt words"), "{screen}");
        },
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn rename_and_delete_with_numbered_answers() {
    let (store, setup) = setup(100).await;
    let model = GatedModel::scripted(vec![text_response(&["current reply"])]);
    with_ui(model, setup, |mut ui| async move {
        ui.submit("current question");
        ui.wait("the turn", |s| idle(s, "current reply")).await;
        ui.submit("/resume");
        ui.wait_for("Alpha task").await;
        // 现在的会话不能删。
        ui.type_text("current");
        wait_chosen(&ui, "the current session", "current question").await;
        ctrl(&ui, 'd');
        ui.wait_for("Use /clear to leave the current session before deleting it.")
            .await;
        // ⌃r 改名：草稿换成名字，⏎ 改，草稿回到 `/resume 词`。
        for _ in 0.."current".len() {
            ui.key(KeyCode::Backspace);
        }
        wait_chosen(&ui, "the first row", "Alpha task").await;
        ctrl(&ui, 'r');
        ui.wait_for("Rename “Alpha task”").await;
        ui.type_text(" v2");
        ui.key(KeyCode::Enter);
        ui.wait("the renamed row", |s| {
            s.contains("Alpha task v2") && !s.contains("Rename “")
        })
        .await;
        // ⌃d：编号的回答，esc 回到列表；再来一次按 1 删。
        wait_chosen(&ui, "the renamed row", "Alpha task v2").await;
        ctrl(&ui, 'd');
        let screen = ui.wait_for("Delete “Alpha task v2”?").await;
        assert!(screen.contains("1  Delete"), "{screen}");
        assert!(screen.contains("2  Keep"), "{screen}");
        ui.key(KeyCode::Esc);
        ui.wait("the question to close", |s| !s.contains("Delete “"))
            .await;
        ctrl(&ui, 'd');
        ui.wait_for("Delete “Alpha task v2”?").await;
        ui.key(KeyCode::Char('1'));
        let screen = ui
            .wait("the row to go", |s| !s.contains("Alpha task v2"))
            .await;
        assert!(screen.contains("beta prompt words"), "{screen}");
    })
    .await;
    let alpha = SessionId::new("sess_alpha");
    assert_eq!(store.title(&alpha).as_deref(), Some("Alpha task v2"));
    assert!(store.is_deleted(&alpha));
}

/// 别的文件夹的会话：先问在哪里打开。会话的文件夹（1 或 ⏎）：工作区是会话的目录；这个文件夹（2）：打开后经 `/cd` 换到
/// 当前目录（只对这次打开有效，tui-manage.md §10.5）。
async fn resume_other(current: bool) -> String {
    let (_, mut setup) = setup(100).await;
    setup.environments = environments(super::sessions_seed::env());
    let model = GatedModel::scripted(Vec::new());
    let (sender, receiver) = std::sync::mpsc::channel();
    with_ui(model, setup, |mut ui| async move {
        // `/resume 词` 在 ⏎ 时按名字恢复；这里在列表开着时打这个词。
        ui.submit("/resume");
        ui.wait_for("Alpha task").await;
        ui.type_text("other");
        ui.key(KeyCode::Tab);
        wait_chosen(&ui, "only the other session", "Other place").await;
        ui.key(KeyCode::Enter);
        let screen = ui.wait_for("Open “Other place” in which folder?").await;
        assert!(screen.contains("1  ~/other"), "{screen}");
        assert!(screen.contains("the session's folder"), "{screen}");
        assert!(screen.contains("2  ~/project"), "{screen}");
        wait_frame(&ui, "the first choice", |shot| coloured(shot, "~/other   ")).await;
        if current {
            ui.key(KeyCode::Char('2'));
            ui.wait_for("Moved to ~/project").await;
        } else {
            ui.key(KeyCode::Enter);
        }
        ui.wait("the other session", |s| {
            s.contains("other reply") && !s.contains("in which folder")
        })
        .await;
        ui.submit("/export chat");
        let screen = ui.wait_for("Exported to ").await;
        sender.send(screen).unwrap();
    })
    .await;
    receiver.recv().unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn another_folder_uses_the_session_folder() {
    let screen = resume_other(false).await;
    assert!(screen.contains("Exported to ~/other/chat.txt"), "{screen}");
}

#[tokio::test(flavor = "multi_thread")]
async fn another_folder_can_use_this_folder() {
    let screen = resume_other(true).await;
    assert!(
        screen.contains("Exported to ~/project/chat.txt"),
        "{screen}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn chinese_steps() {
    let (_, mut setup) = setup(120).await;
    setup.lang = "zh_CN.UTF-8";
    with_ui(
        GatedModel::scripted(Vec::new()),
        setup,
        |mut ui| async move {
            ui.submit("/resume");
            wait_chosen(&ui, "the first row", "Alpha task").await;
            ctrl(&ui, 'd');
            let screen = ui.wait_for("删除“Alpha task”？").await;
            assert!(screen.contains("1  删除"), "{screen}");
            assert!(screen.contains("2  留着"), "{screen}");
            ui.key(KeyCode::Esc);
            ui.wait("the question to close", |s| !s.contains("删除“"))
                .await;
        },
    )
    .await;
}

/// ⌃e（tui-manage.md §10.1）：活动时间（缺省，同 Puffin）与创建时间两种顺序，都是从新到旧；画面上只是行的次序不同。
#[tokio::test(flavor = "multi_thread")]
async fn ctrl_e_orders_by_creation() {
    let (_, setup) = setup(120).await;
    with_ui(
        GatedModel::scripted(Vec::new()),
        setup,
        |mut ui| async move {
            ui.submit("/resume");
            let screen = ui.wait_for("Alpha task").await;
            assert!(
                row(&screen, "Alpha task") < row(&screen, "beta prompt words"),
                "{screen}"
            );
            // 创建时间：beta 比 alpha 新。
            ctrl(&ui, 'e');
            let screen = ui
                .wait("the created order", |s| {
                    s.contains("Alpha task") && row(s, "beta prompt words") < row(s, "Alpha task")
                })
                .await;
            assert!(!screen.contains("Other place"), "{screen}");
            // 有词时搜索也按这个顺序；再按回到活动时间。
            ui.type_text("prompt");
            ui.wait("the created answer", |s| {
                s.contains("Alpha task") && row(s, "beta prompt words") < row(s, "Alpha task")
            })
            .await;
            ctrl(&ui, 'e');
            ui.wait("the activity order", |s| {
                s.contains("beta prompt words")
                    && row(s, "Alpha task") < row(s, "beta prompt words")
            })
            .await;
        },
    )
    .await;
}

/// ⌃f（tui-manage.md §10.1）：全部 → 等你回答 → 在跑 → 打开着空闲 → 没打开 → 全部；筛空时 Puffin 搜不到的那一句写这一步。
#[tokio::test(flavor = "multi_thread")]
async fn ctrl_f_filters_by_status() {
    let (_, setup) = setup(100).await;
    let model = GatedModel::scripted(vec![text_response(&["current reply"])]);
    with_ui(model, setup, |mut ui| async move {
        ui.submit("current question");
        ui.wait("the turn", |s| idle(s, "current reply")).await;
        ui.submit("/resume");
        ui.wait_for("Alpha task").await;
        ctrl(&ui, 'f');
        ui.wait_for("No session matches “needs you”.").await;
        ctrl(&ui, 'f');
        ui.wait_for("No session matches “working”.").await;
        // 打开着空闲：只有现在的会话（对话里也有这句，看选中的行）。
        ctrl(&ui, 'f');
        wait_chosen(&ui, "the open session", "current question").await;
        let screen = ui.frame().text();
        assert!(!screen.contains("Alpha task"), "{screen}");
        assert!(!screen.contains("beta prompt words"), "{screen}");
        // 没打开：库里的两个，选中的是第一个。
        ctrl(&ui, 'f');
        wait_chosen(&ui, "the closed sessions", "Alpha task").await;
        ui.wait_for("beta prompt words").await;
        // 有词时那一句写词与状态。
        ui.type_text("zzz");
        ui.wait_for("No session matches “zzz”, “inactive”.").await;
        for _ in 0..3 {
            ui.key(KeyCode::Backspace);
        }
        // 回到全部。
        ctrl(&ui, 'f');
        ui.wait("every session", |s| {
            s.contains("Alpha task") && s.contains("current question")
        })
        .await;
    })
    .await;
}

/// 这一行去掉两头的空白后正好是 `text`（组名）。启动头的前 3 行不算：它的第 3 行也是文件夹。
fn titled(screen: &str, text: &str) -> Option<usize> {
    let mut rows = screen.lines().enumerate().skip(3);
    rows.find(|(_, line)| line.trim() == text).map(|(at, _)| at)
}

/// ← 与 ⇥ 列所有文件夹时按文件夹分组（tui-manage.md §10.0）：组名同 `@` 面板，最近活动的文件夹在前，组里是现在的顺序
/// （⌃e）与 ⌃f 的筛选，草稿里的词搜索时组还在；只列这个文件夹时没有组名，同 Puffin。
#[tokio::test(flavor = "multi_thread")]
async fn every_folder_groups_the_sessions_under_folder_titles() {
    let (_, setup) = setup(120).await;
    with_ui(
        GatedModel::scripted(Vec::new()),
        setup,
        |mut ui| async move {
            ui.wait_for("? keys").await;
            open_every_folder(&mut ui).await;
            let screen = ui.wait_for("Other place").await;
            let (here, other) = (
                titled(&screen, "~/project").unwrap_or_else(|| panic!("{screen}")),
                titled(&screen, "~/other").unwrap_or_else(|| panic!("{screen}")),
            );
            assert!(
                here < row(&screen, "Alpha task")
                    && row(&screen, "Alpha task") < row(&screen, "beta prompt words")
                    && row(&screen, "beta prompt words") < other
                    && other < row(&screen, "Other place"),
                "{screen}"
            );
            // ⌃e 创建时间：别的文件夹的会话最新，但文件夹仍按最近活动排；组里换成创建的顺序。
            ctrl(&ui, 'e');
            let grouped = |s: &str| {
                let (Some(here), Some(other)) = (titled(s, "~/project"), titled(s, "~/other"))
                else {
                    return false;
                };
                s.contains("Other place")
                    && here < row(s, "beta prompt words")
                    && row(s, "beta prompt words") < row(s, "Alpha task")
                    && row(s, "Alpha task") < other
                    && other < row(s, "Other place")
            };
            ui.wait("the created order inside the folders", grouped)
                .await;
            // 词的回答也分组。
            ui.type_text("prompt");
            ui.wait("the grouped answer", grouped).await;
            // ⌃f 没打开：三个都在，组不变。
            ctrl(&ui, 'f');
            ui.wait_for("No session matches “prompt”, “needs you”.")
                .await;
            for _ in 0..3 {
                ctrl(&ui, 'f');
            }
            ui.wait("the closed sessions, grouped", grouped).await;
            // ⇥ 只列这个文件夹：没有组名。
            ui.key(KeyCode::Tab);
            let screen = ui
                .wait("this folder only", |s| {
                    s.contains("Alpha task") && !s.contains("Other place")
                })
                .await;
            assert_eq!(titled(&screen, "~/project"), None, "{screen}");
        },
    )
    .await;
}

/// 这一行去掉行尾空白后以 `tail` 结束（看板的行尾是状态的字）。
fn ends(screen: &str, text: &str, tail: &str) -> bool {
    screen
        .lines()
        .any(|l| l.contains(text) && l.trim_end().ends_with(tail))
}

/// ← 的看板（tui-panels.md §2）：每行的状态、第一行的数量、选中的会话的分支、首条输入与最后的回复。分支是目录现在的
/// （内核读 `.git/HEAD`）。
#[tokio::test(flavor = "multi_thread")]
async fn every_folder_shows_each_state_the_counts_and_the_details() {
    let (_, mut setup) = setup(120).await;
    let env = super::sessions_seed::env();
    env.set_file("/Users/dev/other/.git/HEAD", "ref: refs/heads/feat/x\n");
    setup.environments = environments(env);
    let model = GatedModel::scripted(vec![text_response(&["current reply"])]);
    with_ui(model, setup, |mut ui| async move {
        ui.submit("current question");
        ui.wait("the turn", |s| idle(s, "current reply")).await;
        open_every_folder(&mut ui).await;
        let screen = ui.wait_for("○ 1 ready   ○ 3 inactive").await;
        // 现在的会话打开着、空闲；库里的三个没打开。
        assert!(
            ends(&screen, "current question", "ready · current"),
            "{screen}"
        );
        assert!(ends(&screen, "Alpha task", "inactive"), "{screen}");
        assert!(ends(&screen, "Other place", "inactive"), "{screen}");
        // 现在的会话（这个文件夹的组里最旧的）：细节行的文件夹、首条输入与最后的回复（搜索时细节行是命中的那段，所以
        // 用 ↓ 走过去）。
        wait_chosen(&ui, "the newest other session", "Alpha task").await;
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Down);
        wait_chosen(&ui, "the current session", "current question").await;
        let screen = ui.wait_for("            › current question").await;
        assert!(screen.contains("            current reply"), "{screen}");
        // 别的文件夹的会话：分支在文件夹后面；没打开的没有回复。
        ui.key(KeyCode::Down);
        wait_chosen(&ui, "the other session", "Other place").await;
        let screen = ui.wait_for("~/other  ·  feat/x").await;
        assert!(screen.contains("› other prompt"), "{screen}");
        assert!(!screen.contains("other reply"), "{screen}");
    })
    .await;
}

/// 现在的会话在跑时在 Working 组；回合结束的事件到了，开着的驾驶舱重读，回到 Resting（tui-cockpit.md §1，不重开）。
#[tokio::test(flavor = "multi_thread")]
async fn the_state_follows_the_current_session_while_the_cockpit_is_open() {
    let (_, setup) = setup(120).await;
    let (model, mut senders) = GatedModel::new(1);
    with_ui(model.clone(), setup, |mut ui| async move {
        ui.submit("long question");
        ui.wait_for("esc stop").await;
        ui.key(KeyCode::Left);
        let screen = ui
            .wait("the cockpit", |s| {
                s.contains("1 working") && s.contains("Working")
            })
            .await;
        assert!(screen.contains("long question  ·  current"), "{screen}");
        assert!(screen.contains("3 resting"), "{screen}");
        super::support::finish(senders.remove(0), text_response(&["long answer"]));
        ui.wait("resting after the turn", |s| {
            s.contains("4 resting") && !s.contains("1 working")
        })
        .await;
    })
    .await;
}

/// ⌃b（tui-panels.md §2.4）：在选中的会话的文件夹新建会话；那个文件夹未信任时先问，拒绝只取消这次，同意后写记录、新建，
/// 输入框上方说 `Started a new session in ~/other.`；下一个回合在那个文件夹里。
#[tokio::test(flavor = "multi_thread")]
async fn ctrl_b_asks_for_trust_then_starts_a_session_in_that_folder() {
    let (_, mut setup) = setup(120).await;
    setup.environments = environments(super::sessions_seed::env());
    let trust = Arc::new(super::sessions_seed::Trust {
        untrusted: vec!["/Users/dev/other"],
        ..super::sessions_seed::Trust::default()
    });
    setup.services = super::sessions_seed::services(&trust);
    let model = GatedModel::scripted(vec![text_response(&["hello from other"])]);
    let checked = trust.clone();
    with_ui(model.clone(), setup, |mut ui| async move {
        ui.wait_for("? keys").await;
        open_every_folder(&mut ui).await;
        ui.wait_for("Other place").await;
        ui.type_text("other");
        wait_chosen(&ui, "the other session", "Other place").await;
        // 拒绝：只取消这次，不新建。
        ctrl(&ui, 'b');
        ui.wait_for("Accessing workspace").await;
        ui.key(KeyCode::Enter);
        let screen = ui
            .wait("the ask to close", |s| !s.contains("Accessing workspace"))
            .await;
        assert!(!screen.contains("Started a new session"), "{screen}");
        assert!(checked.accepted.lock().unwrap().is_empty());
        // 同意：写记录、新建。
        open_every_folder(&mut ui).await;
        ui.wait_for("Other place").await;
        ui.type_text("other");
        wait_chosen(&ui, "the other session", "Other place").await;
        ctrl(&ui, 'b');
        ui.wait_for("Accessing workspace").await;
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Enter);
        ui.wait_for("Started a new session in ~/other.").await;
        assert_eq!(*checked.accepted.lock().unwrap(), ["/Users/dev/other"]);
        ui.submit("where am I");
        ui.wait("the reply", |s| idle(s, "hello from other")).await;
        ui.submit("/export chat");
        ui.wait_for("Exported to ~/other/chat.txt").await;
    })
    .await;
    // 新会话：请求里没有原来会话的历史。
    let text = super::support::request_text(&model.requests()[0]);
    assert!(
        text.contains("where am I") && !text.contains("alpha prompt"),
        "{text}"
    );
}

/// ⌃b 换掉新会话的草稿：草稿里的 `/rename` 随它丢掉，不发给 ⌃b 建的会话（titles.md §4）。修复（MR !3465 评审 CR-01）：
/// 以前只有 `/clear` 清草稿里的命令，⌃b 走的 `clear_into` 只重置视图，旧名字从画面上消失，改名命令却在建会话后照样发出。
#[tokio::test(flavor = "multi_thread")]
async fn ctrl_b_drops_a_rename_held_in_the_old_draft() {
    use rust_cli_kernel_api::store::Fact;
    let (store, mut setup) = setup(120).await;
    setup.environments = environments(super::sessions_seed::env());
    let trust = Arc::new(super::sessions_seed::Trust::default());
    setup.services = super::sessions_seed::services(&trust);
    let model = GatedModel::scripted(vec![text_response(&["hello from other"])]);
    let session = with_ui(model.clone(), setup, |mut ui| async move {
        ui.wait_for("? keys").await;
        ui.submit("/rename old-name");
        ui.wait_for("Session renamed to: old-name").await;
        open_every_folder(&mut ui).await;
        ui.wait_for("Other place").await;
        ui.type_text("other");
        wait_chosen(&ui, "the other session", "Other place").await;
        ctrl(&ui, 'b');
        ui.wait_for("Started a new session in ~/other.").await;
        ui.submit("where am I");
        ui.wait("the reply", |s| idle(s, "hello from other")).await;
    })
    .await;
    let id = SessionId::new(session.expect("the session id"));
    let facts = store.facts(&id);
    assert!(
        !facts
            .iter()
            .any(|fact| matches!(fact, Fact::TitleChanged { title } if title == "old-name")),
        "{facts:#?}"
    );
    let request = super::support::request_text(&model.requests()[0]);
    assert!(!request.contains("old-name"), "{request}");
    assert!(request.contains("where am I"), "{request}");
}

/// `/resume` 列表上的 ⇥：所有文件夹的看板（以前 ← 打开的，← 现在是驾驶舱，tui-cockpit.md §5）。
async fn open_every_folder(ui: &mut super::support::Ui) {
    ui.type_text("/resume");
    ui.key(KeyCode::Enter);
    ui.wait_for("› /resume").await;
    ui.key(KeyCode::Tab);
}
