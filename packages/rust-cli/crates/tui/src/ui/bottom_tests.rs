//! 排队的消息（Puffin 的栈，tui-kit.md §4.2「排队预览」；以前下面还有立即发送的提示，修复线 F104，照 Puffin 删掉）。命令架
//! 与 `@` 面板的行。
use rust_cli_kernel_api::ids::{CommandId, TurnId};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{
    Delivery, KernelEvent, QueuedInput, SequencedEvent, TurnOutcome,
};
use rust_cli_tui_kit::app::layout::Layout;
use rust_cli_tui_kit::render::StyledLine;

use crate::ui::app::App;
use crate::ui::i18n::Lang;
use crate::ui::test_support::app_in;
use crate::view::Transcript;

const WIDTH: usize = 80;
/// 栈上的一条：右沿是 `next turn`（Puffin 的栈）。
const ROW: &str = " › also check the date                                                next turn";

fn selection() -> ModelSelection {
    ModelSelection::new("fixture", "scripted")
}

/// 按顺序把事件折进视图。
fn feed(app: &mut App, events: Vec<KernelEvent>) {
    for (index, event) in events.into_iter().enumerate() {
        let seq = index as u64 + 1;
        app.view.apply(
            SequencedEvent {
                seq,
                revision: 0,
                event,
            },
            seq * 1000,
        );
    }
}

fn started() -> KernelEvent {
    KernelEvent::TurnStarted {
        turn: TurnId::new("t1"),
        command: None,
    }
}

fn finished() -> KernelEvent {
    KernelEvent::TurnFinished {
        turn: TurnId::new("t1"),
        outcome: TurnOutcome::Success,
        usage: None,
    }
}

fn queued(text: &str) -> KernelEvent {
    KernelEvent::QueueChanged {
        items: vec![QueuedInput {
            command: CommandId::new("c2"),
            text: text.to_owned(),
            // 等下一轮的（栈只放它们；插进这一回合的在 transcript 的尾巴里，tui-kit.md §4.0）。
            delivery: Delivery::Queue,
        }],
    }
}

/// 排好的队列区域。
fn laid(app: &App) -> Vec<StyledLine> {
    super::queue(app, WIDTH)
        .iter()
        .flat_map(|row| Layout::lay(row, WIDTH))
        .collect()
}

/// 画出来的队列区域，一行一个字符串。
fn lines(app: &App) -> Vec<String> {
    laid(app)
        .iter()
        .map(|line| line.plain().trim_end().to_owned())
        .collect()
}

/// 回合在跑、栈上有消息：只有栈的行（Puffin 的栈）。以前下面还有一行 fg3 的 `ctrl+x ctrl+s to send now`（修复线 F104），
/// 照 Puffin 删掉；立即发送的键在 `?` 按键表里。
#[test]
fn a_running_turn_shows_only_the_stack() {
    let mut app = app_in(Lang::En);
    feed(&mut app, vec![started(), queued("also check the date")]);
    assert_eq!(lines(&app), [ROW.to_owned()]);
    let mut zh = app_in(Lang::Zh);
    feed(&mut zh, vec![started(), queued("再看看日期")]);
    assert_eq!(lines(&zh).len(), 1, "{:?}", lines(&zh));
}

/// 空闲时栈照旧；队列空了、回合结束之后栈也空了。
#[test]
fn the_stack_follows_the_queue() {
    let mut idle = app_in(Lang::En);
    feed(&mut idle, vec![queued("also check the date")]);
    assert_eq!(lines(&idle), [ROW.to_owned()]);

    let mut running = app_in(Lang::En);
    feed(&mut running, vec![started()]);
    assert!(lines(&running).is_empty(), "no queue, no hint");

    let mut done = app_in(Lang::En);
    feed(
        &mut done,
        vec![
            started(),
            queued("also check the date"),
            KernelEvent::QueueChanged { items: Vec::new() },
            finished(),
        ],
    );
    assert!(
        lines(&done).is_empty(),
        "the queue drained and the turn ended"
    );
}

/// 只读查看子代理时栈照旧是主会话的。
#[test]
fn viewing_a_subagent_keeps_the_stack() {
    let mut app = app_in(Lang::En);
    feed(&mut app, vec![started(), queued("also check the date")]);
    app.viewing = Some(crate::ui::agents::Viewing {
        agent: rust_cli_kernel_api::ids::AgentId::new("a1"),
        kind: "explore".into(),
        description: "look around".into(),
        version: 0,
        status: rust_cli_kernel_api::panels::SubagentStatus::Running,
        view: Transcript::new(
            selection(),
            PermissionMode::Default,
            crate::view::test_workspace(),
        ),
        messages: Vec::new(),
        owner: None,
    });
    assert_eq!(lines(&app), [ROW.to_owned()]);
}

/// 栈（Puffin `stack_rows`）：第 1 列 fg3 的 `›`、文字 fg2，换行画成空格；多于四条时最上面一行是 `N earlier`，只留最后三条。
/// 有插话（Steer）时不写 `next turn`（上面几个用例）。
#[test]
fn the_queue_is_puffins_stack() {
    let mut app = app_in(Lang::En);
    let items = (1..=6)
        .map(|i| QueuedInput {
            command: CommandId::new(format!("c{i}")),
            text: format!("message {i}\nline two"),
            delivery: Delivery::Queue,
        })
        .collect();
    feed(&mut app, vec![KernelEvent::QueueChanged { items }]);
    assert_eq!(
        lines(&app),
        [
            // 都等下一轮：最上面一行的槽里写 `next turn`，最后一格在第 宽度 − 2 列。
            format!("   3 earlier{}next turn", " ".repeat(WIDTH - 1 - 12 - 9)).as_str(),
            " › message 4 line two",
            " › message 5 line two",
            " › message 6 line two",
        ]
    );
    let row = &laid(&app)[1];
    let style = |text: &str| row.spans.iter().find(|s| s.text == text).unwrap().style;
    assert_eq!(style("›"), app.ink.theme.fg3);
    assert_eq!(style("message 4 line two"), app.ink.theme.fg2);
    app.lang = Lang::Zh;
    assert!(lines(&app)[0].starts_with("   更早的 3 条"));
    assert!(lines(&app)[0].ends_with("下一轮"));
}

/// `@` 面板（Puffin `at/rows.rs`，kit 的 `at_panel::rows`）：回答到之前组名下是「在找」；到了之后组名 fg3 在第 3 列，名字在第 5
/// 列补到 24 列（选中的是 file 色），文件夹 fg3；宽的终端在第 51 列起是选中的预览。
#[test]
fn the_file_list_is_puffins_mention_panel() {
    use crate::ui::at_panel::AtFiles;
    use crate::ui::at_search::{FileHit, FileSearch};
    const WIDE: usize = 120;
    let mut app = app_in(Lang::En);
    app.composer.set_text("look at @foo");
    app.composer.refresh_popup();
    let rows = |app: &App| -> Vec<StyledLine> {
        super::popup(app, WIDE)
            .iter()
            .flat_map(|(row, _)| Layout::lay(row, WIDE))
            .collect()
    };
    let text = |rows: &[StyledLine]| -> Vec<String> {
        rows.iter()
            .map(|l| l.plain().trim_end().to_owned())
            .collect()
    };
    assert_eq!(
        text(&rows(&app)),
        ["   Files", "     Looking through the files…"]
    );
    let hit = |path: &str| FileHit {
        path: path.to_owned(),
        folder: false,
    };
    app.composer.files.found(
        AtFiles {
            query: "foo".to_owned(),
            under: String::new(),
            found: FileSearch {
                hits: vec![hit("src/ui/footer.rs"), hit("README.foo")],
                total: 2,
            },
        },
        true,
    );
    let laid = rows(&app);
    assert_eq!(
        text(&laid),
        [
            format!(
                "   Files{}footer.rs",
                " ".repeat(rust_cli_tui_kit::app::widgets::at_panel::rows::PANE_COL - 8)
            ),
            format!(
                "     footer.rs{}src/ui{}src/ui/",
                " ".repeat(24 - 9 + 2),
                " ".repeat(17 - 6 + 3)
            ),
            "     README.foo".to_owned(),
        ]
    );
    let chosen = laid[1]
        .spans
        .iter()
        .find(|s| s.text == "footer.rs")
        .unwrap();
    assert_eq!(chosen.style.fg_color(), app.ink.theme.file);
}

/// 栈与草稿行之间空一行（tui-kit.md §4.0「板」）；栈在没有层的一摞里，所以这一行与栈一起把正文推上去。以前栈紧贴草稿。
#[test]
fn the_stack_keeps_a_blank_row_above_the_draft() {
    let mut without = app_in(Lang::En);
    feed(&mut without, vec![started()]);
    let now = without.started;
    let empty = crate::ui::pane::bottom(&mut without, WIDTH, 30, now, 0, None, false);
    let mut with = app_in(Lang::En);
    feed(&mut with, vec![started(), queued("also check the date")]);
    let now = with.started;
    let stacked = crate::ui::pane::bottom(&mut with, WIDTH, 30, now, 0, None, false);
    assert_eq!(stacked.stack0, empty.stack0 + 2, "the row and the gap");
    let (draft, _, _) = stacked.composer.expect("the draft is on the board");
    let shown: Vec<String> = stacked
        .shelf
        .iter()
        .map(|line| line.plain().trim_end().to_owned())
        .collect();
    assert_eq!(shown[draft - 1], "", "{shown:?}");
    assert_eq!(shown[draft - 2], ROW, "{shown:?}");
    assert_eq!(
        stacked.shelf.len(),
        stacked.stack0,
        "no layer, nothing covers"
    );
}

/// 层放不下时空行让给层的字：80×24 的 `?` 按键表不丢标题（tui-kit.md §4.0「板」）。放得下的高度上照常空一行。
#[test]
fn a_layer_that_does_not_fit_gives_up_the_blank_row() {
    use crate::ui::app::Overlay;
    let mut app = app_in(Lang::En);
    app.overlay = Some(Overlay::Shortcuts);
    let now = app.started;
    let blank_above_draft = |app: &mut App, rows: usize| {
        let bottom = crate::ui::pane::bottom(app, WIDTH, rows, now, 0, None, false);
        let (draft, _, _) = bottom.composer.expect("the draft is on the board");
        bottom.shelf[draft - 1].plain().trim_end().is_empty()
    };
    assert!(blank_above_draft(&mut app, 60));
    assert!(!blank_above_draft(&mut app, 24));
}
