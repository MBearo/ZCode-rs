//! 任务卡（tui-tasks.md §3）：落在哪个种类、种类一行的墨与窄的写法、空的种类被跳过、Commands 的展开与说明、Todos 的窗口、
//! 槽、按键。
use std::time::Instant;

use crossterm::event::{KeyCode, KeyModifiers};
use rust_cli_kernel_api::env_shell::BackgroundStatus;
use rust_cli_kernel_api::panels::{ShellDetail, ShellOutput, SubagentStatus};
use rust_cli_tui_kit::render::{Style, StyledLine, attr};

use super::*;
use crate::ui::i18n::Lang;
use crate::ui::task_line::tests::{app, entry, set, shell, todos};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn plain(lines: &[StyledLine]) -> Vec<String> {
    lines
        .iter()
        .map(|line| line.plain().trim_end().to_owned())
        .collect()
}

fn card(app: &App) -> TaskCard {
    app.task_card().cloned().expect("the card is open")
}

fn slot(app: &App, width: usize) -> String {
    let layer = app.tasks_layer(width).unwrap();
    layer.slot.iter().map(|s| &*s.text).collect()
}

fn style_of(line: &StyledLine, text: &str) -> Style {
    line.spans
        .iter()
        .find(|s| s.text == text)
        .unwrap_or_else(|| panic!("{text:?} not in {:?}", line.plain()))
        .style
}

/// 两个后台子代理在跑、一个命令在跑、待办 1/3。
fn busy(lang: Lang) -> App {
    let mut app = app(lang);
    set(
        &mut app,
        vec![
            entry("a", true, SubagentStatus::Running),
            entry("b", true, SubagentStatus::Running),
        ],
    );
    app.snapshots.background = vec![shell("b1", "npm run dev")];
    todos(&mut app, 1, 3);
    app
}

/// 有等你回答的落在 Subagents；否则上次看的种类；否则第一个有东西的。三个都空时说一句，到下一个键。
#[test]
fn the_card_lands_on_the_kind_that_matters() {
    let mut app = app(Lang::En);
    let now = Instant::now();
    app.on_key(ctrl('t'), now);
    assert!(app.task_card().is_none());
    assert!(app.no_tasks);
    app.on_key(key(KeyCode::Char('x')), now);
    assert!(!app.no_tasks);
    assert_eq!(app.composer.area.text(), "x");
    app.composer.set_text("");
    app.snapshots.background = vec![shell("b1", "npm run dev")];
    todos(&mut app, 1, 3);
    app.on_key(ctrl('t'), now);
    assert_eq!(card(&app).kind, TaskKind::Commands);
    app.on_key(key(KeyCode::Right), now);
    assert_eq!(card(&app).kind, TaskKind::Todos);
    app.on_key(key(KeyCode::Esc), now);
    assert!(app.task_card().is_none());
    app.on_key(ctrl('t'), now);
    assert_eq!(card(&app).kind, TaskKind::Todos);
    app.on_key(ctrl('t'), now);
    assert!(app.task_card().is_none());
    let mut waiting = entry("a", true, SubagentStatus::Running);
    waiting.waiting = true;
    set(&mut app, vec![waiting]);
    app.on_key(ctrl('t'), now);
    assert_eq!(card(&app).kind, TaskKind::Subagents);
}

/// 点任务行（tui-tasks.md §2.2「点击」）：一个部分落在它的种类；记号与 `^T` 同 ^T；那个种类空了时照 ^T；草稿不空时
/// 什么也不做（点在画好之后才到，草稿可能已经有字）。
#[test]
fn a_click_on_the_task_line_lands_on_its_kind() {
    let mut app = busy(Lang::En);
    app.click_tasks(Some(TaskKind::Todos));
    assert_eq!(card(&app).kind, TaskKind::Todos);
    app.overlay = None;
    app.click_tasks(Some(TaskKind::Commands));
    assert_eq!(card(&app).kind, TaskKind::Commands);
    app.overlay = None;
    // 同 ^T：落在上次看的种类。
    app.click_tasks(None);
    assert_eq!(card(&app).kind, TaskKind::Commands);
    app.overlay = None;
    // 命令刚结束、那一处还画着：照 ^T，上次看的种类空了，落在第一个有东西的。
    app.snapshots.background.clear();
    app.click_tasks(Some(TaskKind::Commands));
    assert_eq!(card(&app).kind, TaskKind::Subagents);
    app.overlay = None;
    app.composer.set_text("draft");
    app.click_tasks(Some(TaskKind::Todos));
    assert!(app.task_card().is_none());
}

/// ↓ 只在输入框为空、有东西可看时打开（兼容的旧键）；`/tasks` 也打开。
#[test]
fn down_and_the_command_open_it_too() {
    let mut app = app(Lang::En);
    let now = Instant::now();
    app.on_key(key(KeyCode::Down), now);
    assert!(app.task_card().is_none() && !app.no_tasks);
    app.snapshots.background = vec![shell("b1", "npm run dev")];
    app.on_key(key(KeyCode::Down), now);
    assert_eq!(card(&app).kind, TaskKind::Commands);
}

/// 种类一行：选中的 brand 粗体与数，其余 fg2 与 fg3 的数，空的 quiet 没有数，line 色的 `  ·  `；Todos 选中时右沿是
/// `1 of 3 done`。放不下时换成两个空格，再放不下去掉数。
#[test]
fn the_kinds_row_reads_like_puffins_card() {
    let mut app = busy(Lang::En);
    app.snapshots.background.clear();
    app.open_tasks();
    let theme = app.ink.theme;
    let lines = app.tasks_layer(80).unwrap().lines;
    assert_eq!(
        plain(&lines)[0],
        "   Subagents 2  ·  Commands  ·  Todos 1/3"
    );
    assert_eq!(plain(&lines)[1], "");
    let row = &lines[0];
    assert_eq!(
        style_of(row, "Subagents"),
        Style::fg(theme.brand).with_attr(attr::BOLD)
    );
    assert_eq!(style_of(row, " 2"), Style::fg(theme.brand));
    assert_eq!(style_of(row, "Commands"), theme.quiet);
    assert_eq!(style_of(row, "Todos"), theme.fg2);
    assert_eq!(style_of(row, " 1/3"), theme.fg3);
    assert_eq!(style_of(row, "  ·  "), theme.line);
    app.on_key(key(KeyCode::Right), Instant::now());
    let text = plain(&app.tasks_layer(80).unwrap().lines).remove(0);
    assert_eq!(
        text,
        format!(
            "{:<68}1 of 3 done",
            "   Subagents 2  ·  Commands  ·  Todos 1/3"
        )
    );
    let mut app = busy(Lang::Zh);
    app.open_tasks();
    let at = |app: &App, width| plain(&app.tasks_layer(width).unwrap().lines).remove(0);
    assert_eq!(at(&app, 60), "   子代理 2  ·  命令 1  ·  待办 1/3");
    assert_eq!(at(&app, 30), "   子代理 2  命令 1  待办 1/3");
    assert_eq!(at(&app, 24), "   子代理  命令  待办");
}

/// 等你回答的子代理在 Subagents 的数后面接 code 色的 ` ●`。
#[test]
fn waiting_subagents_mark_the_kind() {
    let mut app = busy(Lang::En);
    app.snapshots.agents.entries[0].waiting = true;
    app.open_tasks();
    let lines = app.tasks_layer(80).unwrap().lines;
    assert!(
        plain(&lines)[0].starts_with("   Subagents 1 ●  ·  Commands 1"),
        "{:?}",
        plain(&lines)
    );
    assert_eq!(style_of(&lines[0], " ●"), Style::fg(app.ink.theme.code));
}

/// ←→ 回绕、跳过空的种类；换种类时展开的输出收起。种类空了时换到下一个，都空了时卡关上。
#[test]
fn arrows_skip_empty_kinds_and_the_card_follows_the_snapshot() {
    let mut app = app(Lang::En);
    let now = Instant::now();
    set(&mut app, vec![entry("a", true, SubagentStatus::Running)]);
    todos(&mut app, 1, 3);
    app.open_tasks();
    assert_eq!(card(&app).kind, TaskKind::Subagents);
    app.on_key(key(KeyCode::Right), now);
    assert_eq!(card(&app).kind, TaskKind::Todos);
    app.on_key(key(KeyCode::Right), now);
    assert_eq!(card(&app).kind, TaskKind::Subagents);
    app.on_key(key(KeyCode::Left), now);
    assert_eq!(card(&app).kind, TaskKind::Todos);
    assert_eq!(slot(&app, 80), "←→ kind");
    app.view = crate::view::Transcript::new(
        rust_cli_kernel_api::testing::selection(),
        rust_cli_kernel_api::permission::PermissionMode::Default,
        crate::view::test_workspace(),
    );
    app.settle_tasks();
    assert_eq!(card(&app).kind, TaskKind::Subagents);
    app.snapshots.agents.entries.clear();
    app.settle_tasks();
    assert!(app.task_card().is_none());
}

/// Commands：`○`、命令的第一行（选中的 brand）、远端的 `@机器`、重连中的 ` · reconnecting`；⏎ 在行下面展开输出的最后 5 行
/// 与说明，再按收起；槽跟着换。
#[test]
fn commands_unfold_their_output_in_place() {
    let mut app = app(Lang::En);
    let now = Instant::now();
    let mut remote = shell("b2", "make data\nsecond line");
    remote.machine = rust_cli_kernel_api::ids::MachineName::new("dev-a");
    remote.reconnecting = true;
    app.snapshots.background = vec![shell("b1", "npm test"), remote];
    app.open_tasks();
    let layer = app.tasks_layer(80).unwrap();
    assert_eq!(
        plain(&layer.lines),
        [
            "   Subagents  ·  Commands 2  ·  Todos",
            "",
            "   ○ npm test",
            "   ○ make data @dev-a · reconnecting",
        ]
    );
    assert_eq!(layer.targets, [(2, 0), (3, 1)]);
    assert_eq!(style_of(&layer.lines[2], "npm test"), app.ink.selection());
    assert_eq!(style_of(&layer.lines[3], "make data"), app.ink.theme.fg2);
    assert_eq!(
        style_of(&layer.lines[3], "@dev-a").fg_color(),
        app.ink.accent()
    );
    assert_eq!(slot(&app, 80), "⏎ output");
    assert_eq!(
        app.on_key(key(KeyCode::Enter), now),
        Some(Action::Panel(Panel::ReadShell("b1".into())))
    );
    app.shell_reading("b1", 1);
    let text = plain(&app.tasks_layer(80).unwrap().lines);
    assert_eq!(text[3], "     │ loading...");
    let lines: Vec<String> = (1..=10).map(|n| format!("line {n}")).collect();
    app.on_shell_detail(
        "b1",
        1,
        Some(ShellDetail {
            id: "b1".into(),
            command: "npm test".into(),
            status: BackgroundStatus::Running,
            elapsed_ms: Some(250_000),
            output: ShellOutput::Tail { lines, total: 214 },
        }),
    );
    let layer = app.tasks_layer(80).unwrap();
    let text = plain(&layer.lines);
    assert_eq!(text[3], "     │ line 6");
    assert_eq!(text[7], "     │ line 10");
    assert_eq!(text[8], "     │ 4m 10s · showing 5 of 214 lines");
    assert_eq!(text[9], "   ○ make data @dev-a · reconnecting");
    assert_eq!(layer.targets, [(2, 0), (9, 1)]);
    assert_eq!(style_of(&layer.lines[3], "│"), app.ink.theme.line);
    assert_eq!(style_of(&layer.lines[3], "line 6"), app.ink.theme.fg3);
    assert_eq!(
        style_of(&layer.lines[8], "4m 10s · showing 5 of 214 lines"),
        app.ink.theme.quiet
    );
    assert_eq!(slot(&app, 80), "⏎ close output");
    // 换选中的行时收起。
    app.on_key(key(KeyCode::Down), now);
    assert_eq!(card(&app).peek, None);
    assert_eq!(card(&app).command, 1);
    app.on_key(key(KeyCode::Up), now);
    app.on_key(key(KeyCode::Enter), now);
    assert!(card(&app).peek.is_some());
    assert_eq!(app.on_key(key(KeyCode::Enter), now), None);
    assert_eq!(card(&app).peek, None);
}

/// 输出的另外几种：没有输出、太大、读不到；全部显示时只写行数；中文的字。
#[test]
fn the_peek_note_covers_every_case() {
    let mut app = app(Lang::En);
    app.snapshots.background = vec![shell("b1", "npm test")];
    app.open_tasks();
    app.task_card_mut().unwrap().peek = Some(ShellView::new("b1".into()));
    let mut reply = ShellDetail {
        id: "b1".into(),
        command: "npm test".into(),
        status: BackgroundStatus::Completed,
        elapsed_ms: None,
        output: ShellOutput::Tail {
            lines: Vec::new(),
            total: 0,
        },
    };
    let note = |app: &mut App, reply: &ShellDetail| {
        app.on_shell_detail("b1", 0, Some(reply.clone()));
        plain(&app.tasks_layer(80).unwrap().lines).pop().unwrap()
    };
    assert_eq!(note(&mut app, &reply), "     │ No output yet");
    reply.output = ShellOutput::Tail {
        lines: vec!["a".into(), "b".into()],
        total: 2,
    };
    assert_eq!(note(&mut app, &reply), "     │ 2 lines");
    reply.output = ShellOutput::TooLarge { bytes: 9 };
    assert_eq!(note(&mut app, &reply), "     │ Output too large to preview");
    reply.output = ShellOutput::Unavailable;
    assert_eq!(note(&mut app, &reply), "     │ Output unavailable");
    app.lang = Lang::Zh;
    reply.output = ShellOutput::Tail {
        lines: (1..=9).map(|n| n.to_string()).collect(),
        total: 9,
    };
    assert_eq!(note(&mut app, &reply), "     │ 显示 9 行中的 5 行");
}

/// Todos：整个清单，每步一行（kit `todo_step`），没有选中的行；多于 16 行时 ↓ 滚动，藏起的行用 `↑ N more` / `↓ N more`。
#[test]
fn todos_list_every_step_in_a_scrolling_window() {
    let mut app = app(Lang::En);
    let now = Instant::now();
    todos(&mut app, 2, 20);
    app.open_tasks();
    let layer = app.tasks_layer(80).unwrap();
    let text = plain(&layer.lines);
    assert_eq!(text.len(), 2 + crate::ui::task_card_rows::WINDOW);
    assert_eq!(text[2], "   ✓ Step 1");
    assert_eq!(text[4], "   · Step 3");
    assert_eq!(text[17], "     ↓ 5 more");
    assert!(layer.targets.is_empty());
    assert_eq!(slot(&app, 80), "");
    for _ in 0..10 {
        app.on_key(key(KeyCode::Down), now);
    }
    assert_eq!(card(&app).todo_top, 4);
    let text = plain(&app.tasks_layer(80).unwrap().lines);
    assert_eq!(text[2], "     ↑ 5 more");
    assert_eq!(text[3], "   · Step 6");
    assert_eq!(text[17], "   · Step 20");
}

/// Subagents：第一项是 main，然后是树；打开时选中第一个子代理（§3.2）；⏎ 查看选中的子代理，卡留着（§3.3）；`^X` 停止在跑的。
#[test]
fn subagents_are_viewed_and_stopped_from_the_card() {
    let mut app = busy(Lang::En);
    let now = Instant::now();
    app.open_tasks();
    let layer = app.tasks_layer(80).unwrap();
    assert_eq!(
        layer.targets.iter().map(|t| t.1).collect::<Vec<_>>(),
        [0, 1, 2]
    );
    assert_eq!(app.agent_row(&card(&app)), 1);
    assert_eq!(slot(&app, 80), "←→ kind  ⏎ view");
    assert_eq!(
        app.on_key(ctrl('x'), now),
        Some(Action::Manage(Manage::StopTask {
            id: "a".into(),
            label: "Task a".into()
        }))
    );
    assert!(app.task_card().is_some());
    assert_eq!(
        app.on_key(key(KeyCode::Enter), now),
        Some(Action::Panel(Panel::ViewAgent(Some(
            rust_cli_kernel_api::ids::AgentId::new("a")
        ))))
    );
    assert_eq!(app.agent_row(&card(&app)), 1);
    // main 上（没在查看子代理）⏎ 没有事做，槽不写 `⏎ view`。
    app.on_key(key(KeyCode::Up), now);
    assert_eq!(app.agent_row(&card(&app)), 0);
    assert_eq!(slot(&app, 80), "←→ kind");
}

/// 命令的 `^X` 停止它；别的键关上卡、照常处理（字进草稿）。
#[test]
fn ctrl_x_stops_a_command_and_typing_closes_the_card() {
    let mut app = app(Lang::En);
    let now = Instant::now();
    app.snapshots.background = vec![shell("b1", "npm test")];
    app.open_tasks();
    assert_eq!(
        app.on_key(ctrl('x'), now),
        Some(Action::Manage(Manage::StopTask {
            id: "b1".into(),
            label: "npm test".into()
        }))
    );
    app.on_key(key(KeyCode::Char('h')), now);
    assert!(app.task_card().is_none());
    assert_eq!(app.composer.area.text(), "h");
}

/// 卡的最后一行与草稿行之间空一行（tui-kit.md §4.0「板」）；这一行是层的一部分，不推正文。以前卡的最后一行紧贴草稿。
#[test]
fn the_card_keeps_a_blank_row_above_the_draft() {
    let mut app = busy(Lang::En);
    let now = app.started;
    let closed = crate::ui::pane::bottom(&mut app, 80, 30, now, 0, None, false);
    app.on_key(ctrl('t'), now);
    let open = crate::ui::pane::bottom(&mut app, 80, 30, now, 0, None, false);
    assert_eq!(
        open.stack0, closed.stack0,
        "the card does not push the body"
    );
    let (draft, _, _) = open.composer.expect("the draft is on the board");
    let shown = plain(&open.shelf);
    assert!(shown[draft].contains('›'), "{shown:?}");
    assert_eq!(shown[draft - 1], "", "{shown:?}");
    assert_ne!(shown[draft - 2], "", "{shown:?}");
    // 卡的行的点击目标不受这一行影响：最后一个目标还在它自己的行上，草稿行没有卡的目标。
    let last = open
        .targets
        .iter()
        .map(|(row, ..)| *row)
        .max()
        .expect("the card has rows to click");
    assert!(last < draft - 1, "{last} {draft}");
}
