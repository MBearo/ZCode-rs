//! 任务行（tui-tasks.md §2）：数从快照来、哪些子代理算、部分的字与墨、记号、窄的时候、中文、动效。
use std::time::{Duration, Instant};

use rust_cli_kernel_api::env_shell::BackgroundStatus;
use rust_cli_kernel_api::ids::{MessageId, SessionId};
use rust_cli_kernel_api::manage::{BackgroundEntry, BackgroundKind};
use rust_cli_kernel_api::message::{ContentBlock, Message, UserMessage, UserOrigin};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{KernelEvent, SequencedEvent};
use rust_cli_kernel_api::testing::selection;

use super::*;
use crate::ui::probe::{ColorLevel, Colors};
use crate::view::Transcript;

/// 快照到达的墙上时间（Unix 毫秒）。
const T0: u64 = 1_800_000_000_000;

pub(crate) fn app(lang: Lang) -> App {
    App::new(
        // 报告了前景，主题是 rich（淡入只在 rich 下有，tui-kit.md §3.4）。
        Colors::new(ColorLevel::TrueColor, Some((0, 0, 0))).with_foreground(Some((255, 255, 255))),
        lang,
        Transcript::new(
            selection(),
            PermissionMode::Default,
            crate::view::test_workspace(),
        ),
        Some("/Users/dev".into()),
        "0.1.0".into(),
    )
}

pub(crate) fn entry(id: &str, background: bool, status: SubagentStatus) -> SubagentEntry {
    SubagentEntry {
        agent: AgentId::new(id),
        child: SessionId::new(format!("sess_subagent_{id}")),
        agent_type: "Explore".into(),
        description: format!("Task {id}"),
        status,
        background,
        waiting: false,
        elapsed_ms: 185_000,
        ended_ago_ms: None,
        version: 0,
        owner: None,
        activity: None,
        parked: false,
        machine: Default::default(),
    }
}

pub(crate) fn shell(id: &str, command: &str) -> BackgroundEntry {
    BackgroundEntry::new(
        id,
        BackgroundKind::Shell,
        command,
        BackgroundStatus::Running,
    )
}

/// 快照在 T0 到达，画面也在 T0。
pub(crate) fn set(app: &mut App, entries: Vec<SubagentEntry>) {
    app.snapshots.agents.entries = entries;
    app.snapshots.agents.at = Some(T0);
    app.snapshots.agents.shown = T0;
}

/// 内核的任务清单（`TasksChanged`），是这个回合写的。`done` 个完成，其余没开始。
pub(crate) fn todos(app: &mut App, done: usize, total: usize) {
    // 每次一个新的序号：视图丢掉序号不新的事件。
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(100);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tasks: Vec<serde_json::Value> = (1..=total)
        .map(|i| {
            let status = if i <= done { "completed" } else { "pending" };
            serde_json::json!({"id": i.to_string(), "subject": format!("Step {i}"), "description": "",
                               "status": status, "blocks": [], "blockedBy": []})
        })
        .collect();
    app.view.apply(
        SequencedEvent {
            seq,
            revision: 0,
            event: KernelEvent::TasksChanged {
                tasks: serde_json::Value::Array(tasks),
            },
        },
        T0,
    );
    app.snapshots.tasks.changed(&app.view);
}

/// 主会话收到了 `agent` 的结果：内核为它开的回合的 meta 消息（`<task-notification>`）。
fn report(app: &mut App, agent: &str) {
    let text = format!(
        "<task-notification>\n<task-id>{agent}</task-id>\n<status>completed</status>\n</task-notification>"
    );
    app.view.apply(
        SequencedEvent {
            seq: 2,
            revision: 0,
            event: KernelEvent::MessageDone {
                message: Message::User(UserMessage {
                    id: MessageId::new("m1"),
                    content: vec![ContentBlock::Text { text }],
                    origin: UserOrigin::Meta {
                        source: "task-notification".into(),
                    },
                }),
            },
        },
        T0,
    );
}

/// 只有计数的标题行（没有名字）。
fn summary(app: &App, width: usize, at: Instant) -> Vec<StyledLine> {
    app.title_row(width, at, None, true)
        .map(|row| row.line)
        .into_iter()
        .collect()
}

/// 记下这一帧再画；画的时刻在出现之后 1 s（淡入早已结束）。
fn line(app: &mut App, width: usize) -> Vec<StyledLine> {
    let now = Instant::now();
    app.note_dock();
    app.note_line(now);
    summary(app, width, now + Duration::from_secs(1))
}

fn plain(lines: &[StyledLine]) -> Vec<String> {
    lines.iter().map(StyledLine::plain).collect()
}

fn style_of(line: &StyledLine, text: &str) -> Style {
    line.spans
        .iter()
        .find(|s| s.text == text)
        .unwrap_or_else(|| panic!("{text:?} not in {:?}", line.plain()))
        .style
}

/// 数只算后台的：前台子代理与它派生的不算，后台子代理派生的算；等你回答的单独数；命令只数在跑的。
#[test]
fn counts_take_background_work_from_the_snapshot() {
    let mut app = app(Lang::En);
    let mut child = entry("c", false, SubagentStatus::Running);
    child.owner = Some(AgentId::new("a"));
    let mut under_foreground = entry("e", false, SubagentStatus::Running);
    under_foreground.owner = Some(AgentId::new("b"));
    let mut waiting = entry("d", true, SubagentStatus::Running);
    waiting.waiting = true;
    set(
        &mut app,
        vec![
            entry("a", true, SubagentStatus::Running),
            entry("b", false, SubagentStatus::Running),
            child,
            under_foreground,
            waiting,
        ],
    );
    app.snapshots.background = vec![
        shell("x", "npm run dev"),
        BackgroundEntry {
            status: BackgroundStatus::Completed,
            ..shell("y", "ls")
        },
        BackgroundEntry {
            kind: BackgroundKind::Agent,
            ..shell("a", "Task a")
        },
    ];
    let counts = app.task_counts();
    assert_eq!(
        counts,
        TaskCounts {
            waiting: 1,
            running: 2,
            shells: 1,
            ..TaskCounts::default()
        }
    );
    assert_eq!(counts.mark(), Some(LineMark::Waiting));
}

/// 没有任务时没有这一行；有时是 ` 记号 部分 · 部分`，空三格接 `↓/^T all tasks`；数与名词 fg2、动词 fg3。
#[test]
fn the_line_counts_and_points_at_the_card() {
    let mut app = app(Lang::En);
    assert!(line(&mut app, 80).is_empty());
    set(
        &mut app,
        vec![
            entry("a", true, SubagentStatus::Running),
            entry("b", true, SubagentStatus::Running),
        ],
    );
    app.snapshots.background = vec![shell("x", "npm run dev")];
    let lines = line(&mut app, 80);
    let left = " ○ 2 subagents running · 1 shell running";
    assert_eq!(plain(&lines), vec![format!("{left}   ↓/^T all tasks")]);
    let theme = app.ink.theme;
    let l = &lines[0];
    assert_eq!(style_of(l, "2 subagents"), theme.fg2);
    assert_eq!(style_of(l, " running"), theme.fg3);
    assert_eq!(style_of(l, " · "), theme.line);
    assert_eq!(style_of(l, "↓"), theme.fg2);
    assert_eq!(style_of(l, "/"), theme.fg3);
    assert_eq!(style_of(l, "^T"), theme.fg2);
    assert_eq!(style_of(l, " all tasks"), theme.fg3);
    // 回合不在跑：`○` 借工作光标的呼吸。
    assert!(app.line_breathes());
    let tick = i64::try_from(app.snapshots.agents.shown / TICK_MS).unwrap();
    assert_eq!(style_of(l, "○"), caret_style(&theme, tick));
}

/// 中文换字不换排法；待办的部分是 `待办完成 3/8`。
#[test]
fn chinese_words() {
    let mut app = app(Lang::Zh);
    set(&mut app, vec![entry("a", true, SubagentStatus::Running)]);
    app.snapshots.background = vec![shell("x", "npm run dev"), shell("y", "cargo watch")];
    todos(&mut app, 3, 8);
    let lines = line(&mut app, 80);
    let text = &plain(&lines)[0];
    assert!(
        text.starts_with(" ○ 1 个子代理在跑 · 2 shell 在跑 · 待办完成 3/8 "),
        "{text}"
    );
    assert!(text.ends_with("   ↓/^T 全部任务"), "{text}");
}

/// 修复线 F117：命令一段分开数 shell 与 monitor（`1 shell, 2 monitors`；中文 `1 shell、2 monitor`，不带“个”），与回合结束
/// 那一行的 `still running` 同一个写法（`manage::command_text`）。
#[test]
fn the_command_part_counts_monitors_apart_from_shells() {
    let monitor = |id: &str, description: &str| BackgroundEntry {
        kind: BackgroundKind::Monitor,
        ..shell(id, description)
    };
    for (lang, want) in [
        (Lang::En, " ○ 1 shell, 2 monitors running "),
        (Lang::Zh, " ○ 1 shell、2 monitor 在跑 "),
    ] {
        let mut app = app(lang);
        app.snapshots.background = vec![
            shell("x", "npm run dev"),
            monitor("m", "build log"),
            monitor("n", "deploy feed"),
        ];
        assert_eq!(app.task_counts().commands(), 3);
        let text = plain(&line(&mut app, 80)).remove(0);
        assert!(text.starts_with(want), "{text}");
    }
    let mut app = app(Lang::En);
    app.snapshots.background = vec![monitor("m", "build log")];
    let text = plain(&line(&mut app, 80)).remove(0);
    assert!(text.starts_with(" ○ 1 monitor running "), "{text}");
}

/// 没有名字时：放不下才从右往左换成短写，再去掉 `↓/` 与 ` all tasks`，再从后面截；`^T` 一直在。
#[test]
fn narrow_lines_shorten_from_the_right() {
    let mut app = two_agents_and_a_shell();
    let at = |app: &mut App, width| plain(&line(app, width)).remove(0);
    assert_eq!(
        at(&mut app, 58),
        " ○ 2 subagents running · 1 shell running   ↓/^T all tasks"
    );
    assert_eq!(
        at(&mut app, 54),
        " ○ 2 subagents running · 1 shell   ↓/^T all tasks"
    );
    assert_eq!(
        at(&mut app, 48),
        " ○ 2 subagents · 1 shell   ↓/^T all tasks"
    );
    assert_eq!(at(&mut app, 38), " ○ 2 subagents · 1 shell   ^T");
    let cut = at(&mut app, 20);
    assert!(cut.contains('…') && cut.ends_with("   ^T"), "{cut}");
    assert!(string_width(&cut) <= 19, "{cut}");
}

fn two_agents_and_a_shell() -> App {
    let mut app = app(Lang::En);
    set(
        &mut app,
        vec![
            entry("a", true, SubagentStatus::Running),
            entry("b", true, SubagentStatus::Running),
        ],
    );
    app.snapshots.background = vec![shell("x", "npm run dev")];
    app
}

fn name(text: &str, stays: bool, style: Style) -> Title {
    Title {
        text: text.to_owned(),
        style,
        stays,
    }
}

/// 一行里的名字：右对齐到第 `宽度 − 1` 列；左边给它留地方时依次换短写、去掉 `↓/` 与 ` all tasks`，它自己再截，不足 10 列时走开。
#[test]
fn the_name_sits_at_the_right_and_gives_way_last() {
    let mut app = two_agents_and_a_shell();
    let now = Instant::now();
    app.note_dock();
    app.note_line(now);
    let later = now + Duration::from_secs(1);
    let fg3 = app.ink.theme.fg3;
    let at = |app: &App, width| {
        app.title_row(
            width,
            later,
            Some(name("Fix the footer spacing", false, fg3)),
            true,
        )
        .unwrap()
        .line
        .plain()
    };
    let wide = at(&app, 100);
    assert_eq!(
        wide,
        format!(
            "{:<77}Fix the footer spacing",
            " ○ 2 subagents running · 1 shell running   ↓/^T all tasks"
        )
    );
    assert_eq!(string_width(&wide), 99);
    // 还留得出 16 列：计数全写，名字截到剩下的列。
    let cut = at(&app, 80);
    assert!(
        cut.starts_with(" ○ 2 subagents running · 1 shell running   ↓/^T all tasks   "),
        "{cut}"
    );
    assert!(cut.ends_with('…'), "{cut}");
    assert_eq!(string_width(&cut), 79);
    // 留不出：计数短写，再去掉 `↓/` 与 ` all tasks`。
    assert!(at(&app, 70).starts_with(" ○ 2 subagents running · 1 shell   ↓/^T all tasks   "));
    assert!(at(&app, 50).starts_with(" ○ 2 subagents · 1 shell   ^T   "));
    // 不足 10 列：名字走开，计数恢复它放得下的写法。
    assert_eq!(at(&app, 42), " ○ 2 subagents · 1 shell   ↓/^T all tasks");
    assert_eq!(at(&app, 38), " ○ 2 subagents · 1 shell   ^T");
    // 没有任务：只有名字。
    let idle = self::app(Lang::En);
    let row = idle
        .title_row(30, later, Some(name("Fix lint", false, fg3)), true)
        .unwrap()
        .line;
    assert_eq!(row.plain(), format!("{}Fix lint", " ".repeat(21)));
    // 开着页：计数不写，名字留着。
    let page = app
        .title_row(30, later, Some(name("Fix lint", false, fg3)), false)
        .unwrap()
        .line;
    assert_eq!(page.plain().trim(), "Fix lint");
    assert_eq!(app.title_row(30, later, None, false), None);
}

/// 子代理的描述不走开：放不下时计数先走，左边只剩记号与 `^T`。
#[test]
fn a_subagent_label_keeps_its_place_and_the_counts_leave() {
    let mut app = two_agents_and_a_shell();
    let now = Instant::now();
    app.note_dock();
    app.note_line(now);
    let later = now + Duration::from_secs(1);
    let ink = app.ink.theme.agent;
    let row = app
        .title_row(
            30,
            later,
            Some(name("F58 differing duplicates", true, Style::fg(ink))),
            true,
        )
        .unwrap()
        .line;
    assert!(row.plain().starts_with(" ○   ^T   "), "{}", row.plain());
    assert!(row.plain().ends_with('…'), "{}", row.plain());
    assert_eq!(string_width(&row.plain()), 29);
    // 普通的名字放不下时自己走开，计数留着。
    let row = app
        .title_row(
            36,
            later,
            Some(name("F58 differing duplicates", false, app.ink.theme.fg3)),
            true,
        )
        .unwrap()
        .line;
    assert_eq!(row.plain(), " ○ 2 subagents · 1 shell   ^T");
}

/// 草稿不空或有请求时 ^T 不起作用：`↓/^T all tasks` 换成 quiet，位置不变。
#[test]
fn the_key_goes_quiet_while_it_does_not_work() {
    let mut app = two_agents_and_a_shell();
    let before = line(&mut app, 80).remove(0);
    app.composer.area.set_text("draft");
    let typed = line(&mut app, 80).remove(0);
    assert_eq!(typed.plain(), before.plain());
    let quiet = app.ink.theme.quiet;
    assert_eq!(style_of(&typed, "↓"), quiet);
    assert_eq!(style_of(&typed, "/"), quiet);
    assert_eq!(style_of(&typed, "^T"), quiet);
    assert_eq!(style_of(&typed, " all tasks"), quiet);
    assert_eq!(style_of(&typed, "2 subagents"), app.ink.theme.fg2);
}

/// 记号是最急的一种：等你 `●` code，出错 `✗` err，完成 `✓` ok，停了 `–` fg3，只有待办 `○` fg3；整段一种墨的部分同色。
#[test]
fn the_mark_is_the_most_urgent_kind() {
    let theme = app(Lang::En).ink.theme;
    let mark_of = |counts: TaskCounts| counts.mark();
    assert_eq!(
        mark_of(TaskCounts {
            errored: 1,
            finished: 2,
            ..TaskCounts::default()
        }),
        Some(LineMark::Errored)
    );
    assert_eq!(
        mark_of(TaskCounts {
            stopped: 1,
            todos: Some((1, 2)),
            ..TaskCounts::default()
        }),
        Some(LineMark::Stopped)
    );
    assert_eq!(
        mark_of(TaskCounts {
            shells: 1,
            errored: 1,
            ..TaskCounts::default()
        }),
        Some(LineMark::Running)
    );
    let mut app = app(Lang::En);
    let mut waiting = entry("a", true, SubagentStatus::Running);
    waiting.waiting = true;
    set(&mut app, vec![waiting]);
    let lines = line(&mut app, 80);
    assert_eq!(style_of(&lines[0], "●"), Style::fg(theme.code));
    assert_eq!(
        style_of(&lines[0], "1 subagent waiting for you"),
        Style::fg(theme.code)
    );
    let mut app = self::app(Lang::En);
    todos(&mut app, 1, 3);
    let lines = line(&mut app, 80);
    assert!(plain(&lines)[0].starts_with(" ○ 1 of 3 todos done "));
    assert_eq!(style_of(&lines[0], "○"), theme.fg3);
}

/// 结束了的顶层后台子代理（界面见过它在跑）数成完成、出错、停了，留到主会话收到它的结果；没见过在跑的不数。
#[test]
fn ended_subagents_count_until_their_result_arrives() {
    let mut app = app(Lang::En);
    set(
        &mut app,
        vec![
            entry("a", true, SubagentStatus::Running),
            entry("b", true, SubagentStatus::Running),
            entry("c", true, SubagentStatus::Running),
            entry("d", true, SubagentStatus::Completed),
        ],
    );
    app.note_dock();
    app.snapshots.agents.entries[0].status = SubagentStatus::Completed;
    app.snapshots.agents.entries[1].status = SubagentStatus::Failed;
    app.snapshots.agents.entries[2].status = SubagentStatus::Killed;
    let lines = line(&mut app, 100);
    let theme = app.ink.theme;
    let text = &plain(&lines)[0];
    assert!(
        text.starts_with(" ✗ 1 subagent finished · 1 subagent errored · 1 subagent stopped "),
        "{text}"
    );
    assert_eq!(
        style_of(&lines[0], "1 subagent finished"),
        Style::fg(theme.ok)
    );
    assert_eq!(
        style_of(&lines[0], "1 subagent errored"),
        Style::fg(theme.err)
    );
    assert_eq!(style_of(&lines[0], "1 subagent stopped"), theme.fg3);
    report(&mut app, "b");
    let text = plain(&line(&mut app, 100)).remove(0);
    assert!(
        text.starts_with(" ✓ 1 subagent finished · 1 subagent stopped "),
        "{text}"
    );
}

/// 出现时从背景色淡入；记号换了时新记号从 line 色渐变；落定后不再重画（没有在跑的东西时不呼吸）。
#[test]
fn the_line_fades_in_and_the_mark_blends_from_line() {
    let mut app = app(Lang::En);
    set(&mut app, vec![entry("a", true, SubagentStatus::Running)]);
    app.note_dock();
    app.snapshots.agents.entries[0].status = SubagentStatus::Completed;
    let t0 = Instant::now();
    app.note_dock();
    app.note_line(t0);
    assert!(app.line_moving(t0));
    let theme = app.ink.theme;
    let fresh = summary(&app, 80, t0);
    assert_ne!(style_of(&fresh[0], "✓"), Style::fg(theme.ok));
    let later = t0 + Duration::from_secs(1);
    assert!(!app.line_moving(later));
    // 出错了：记号换成 `✗`，从 line 色起。
    app.snapshots
        .agents
        .entries
        .push(entry("b", true, SubagentStatus::Running));
    app.note_dock();
    app.snapshots.agents.entries[1].status = SubagentStatus::Failed;
    app.note_dock();
    app.note_line(later);
    let blended = summary(&app, 80, later);
    assert_eq!(
        style_of(&blended[0], "✗"),
        fade::ink(
            &theme,
            theme.line.fg_color(),
            Style::fg(theme.err),
            0,
            TICKS
        )
    );
    assert!(app.line_moving(later));
    let settled = summary(&app, 80, later + Duration::from_secs(1));
    assert_eq!(style_of(&settled[0], "✗"), Style::fg(theme.err));
}

/// 一条用户消息：transcript 有了正文（`pane::has_body`）。
fn with_body(app: &mut App) {
    app.view.apply(
        SequencedEvent {
            seq: 2,
            revision: 0,
            event: KernelEvent::MessageDone {
                message: Message::User(UserMessage {
                    id: MessageId::new("m1"),
                    content: vec![ContentBlock::Text {
                        text: "fix it".into(),
                    }],
                    origin: UserOrigin::Human,
                }),
            },
        },
        T0,
    );
}

fn titled(app: &mut App) {
    app.view.apply(
        SequencedEvent {
            seq: 1,
            revision: 0,
            event: KernelEvent::TitleChanged {
                title: "Fix lint".into(),
            },
        },
        T0,
    );
}

/// 板上面的两行（第二行是标题行）。
fn above_board(app: &mut App, width: usize) -> (usize, Vec<String>) {
    let now = app.started;
    let bottom = crate::ui::pane::bottom(app, width, 30, now, 0, None, false);
    let top = bottom
        .shelf
        .iter()
        .position(|l| l.plain().starts_with('▄'))
        .expect("the board");
    let rows = bottom.shelf[..top].iter().map(StyledLine::plain).collect();
    (bottom.stack0, rows)
}

/// 有正文时板上面总是一行空行与标题行：任务开始、结束时正文不动（tui-tasks.md §2.3）；层开着时标题行不画，层盖住它。
#[test]
fn the_title_row_stays_so_the_transcript_does_not_move() {
    let mut app = app(Lang::En);
    titled(&mut app);
    with_body(&mut app);
    let (idle, rows) = above_board(&mut app, 80);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0].trim(), "", "{rows:?}");
    assert!(rows[1].ends_with("Fix lint"), "{rows:?}");
    // 名字没有了也留着这一行。
    let mut untitled = self::app(Lang::En);
    with_body(&mut untitled);
    let (blank, rows) = above_board(&mut untitled, 80);
    assert_eq!(blank, idle);
    assert_eq!(rows.iter().map(|r| r.trim()).collect::<Vec<_>>(), ["", ""]);
    // 任务开始：还是那两行。
    app.snapshots.background = vec![shell("x", "npm run dev")];
    app.note_dock();
    let (busy, rows) = above_board(&mut app, 80);
    assert_eq!(busy, idle);
    assert!(
        rows[1].starts_with(" ○ 1 shell running   ↓/^T all tasks"),
        "{rows:?}"
    );
    assert!(rows[1].ends_with("Fix lint"), "{rows:?}");
    // 层开着：标题行不画（仍算在没有层的一摞里），名字也不写。
    app.overlay = Some(crate::ui::app::Overlay::Shortcuts);
    let (layered, rows) = above_board(&mut app, 80);
    assert_eq!(layered, idle);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].trim(), "", "{rows:?}");
}

/// 没有正文时（rich）板上面没有行；有任务时只有标题行。提示占着时不写名字。
#[test]
fn without_a_body_the_row_comes_only_with_words() {
    let mut app = app(Lang::En);
    titled(&mut app);
    assert!(above_board(&mut app, 80).1.is_empty());
    app.snapshots.background = vec![shell("x", "npm run dev")];
    app.note_dock();
    let (_, rows) = above_board(&mut app, 80);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(!rows[0].contains("Fix lint"), "{rows:?}");
}
