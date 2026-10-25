//! 后台任务（tui-panels.md §3）：pill 的选中、Background 面板的分组与按键、Shell 详情的读取与排版。
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_kernel_api::ids::{AgentId, SessionId};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::selection;

use super::*;
use crate::ui::agents::AgentList;
use crate::ui::palette::{ColorLevel, Palette};
use crate::view::Transcript;

fn app() -> App {
    App::new(
        Palette::new(ColorLevel::TrueColor, Some((0, 0, 0))),
        Lang::En,
        Transcript::new(
            selection(),
            PermissionMode::Default,
            crate::view::test_workspace(),
        ),
        Some("/Users/dev".into()),
        "0.1.0".into(),
    )
}

fn shell(id: &str, command: &str) -> BackgroundEntry {
    BackgroundEntry {
        id: id.into(),
        kind: BackgroundKind::Shell,
        description: command.into(),
        status: BackgroundStatus::Running,
        output: None,
        machine: None,
        reconnecting: false,
    }
}

fn agent(id: &str) -> SubagentEntry {
    SubagentEntry {
        agent: AgentId::new(id),
        child: SessionId::new(format!("sess_subagent_{id}")),
        agent_type: "general-purpose".into(),
        description: "Review code".into(),
        status: SubagentStatus::Running,
        background: true,
        waiting: false,
        elapsed_ms: 0,
        ended_ago_ms: None,
        version: 0,
        owner: None,
        activity: None,
        parked: false,
        machine: None,
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// 行的文字（选中行的底色条补到整行，去掉行尾的空格）。
fn plain(lines: &[Line<'static>]) -> Vec<String> {
    lines
        .iter()
        .map(|line| crate::ui::text::plain(line).trim_end().to_owned())
        .collect()
}

fn detail(status: BackgroundStatus, lines: &[&str], total: u64) -> ShellDetail {
    ShellDetail {
        id: "b1".into(),
        command: "npm test".into(),
        status,
        elapsed_ms: (status == BackgroundStatus::Running).then_some(83_000),
        output: ShellOutput::Tail {
            lines: lines.iter().map(|l| (*l).to_owned()).collect(),
            total,
        },
    }
}

#[test]
fn down_selects_the_pill_then_the_agent_list() {
    let mut app = app();
    let now = Instant::now();
    app.background = vec![shell("b1", "npm test")];
    app.agents = AgentList {
        entries: vec![agent("agent_a")],
        at: Some(now),
    };
    app.on_key(key(KeyCode::Down), now);
    assert_eq!(app.focus, Some(Focus::Pill));
    app.on_key(key(KeyCode::Down), now);
    assert_eq!(app.focus, Some(Focus::Agents(0)));
    // ↑ 取消；enter 打开面板。
    app.focus = Some(Focus::Pill);
    app.on_key(key(KeyCode::Up), now);
    assert_eq!(app.focus, None);
    app.on_key(key(KeyCode::Down), now);
    app.on_key(key(KeyCode::Enter), now);
    assert!(matches!(app.overlay, Some(Overlay::Background(_))));
}

#[test]
fn the_panel_groups_shells_and_agents() {
    let mut app = app();
    let now = Instant::now();
    app.background = vec![shell("b1", "npm test"), shell("b2", "cargo build")];
    app.agents = AgentList {
        entries: vec![agent("agent_a")],
        at: Some(now),
    };
    app.open_background();
    let (lines, rows) = app.background_lines(80, 30, now);
    let text = plain(&lines);
    assert_eq!(text[1], "  Background");
    assert_eq!(text[2], "  2 active shells · 1 active agent");
    assert_eq!(text[4], "  Shells (2)");
    assert!(
        text[5].starts_with("› ") && text[5].contains("npm test"),
        "{text:?}"
    );
    assert!(text[5].ends_with("running"), "{text:?}");
    assert_eq!(text[8], "  Local agents (1)");
    assert!(text[9].contains("Review code"), "{text:?}");
    assert_eq!(
        text.last().unwrap(),
        "  ↑/↓ to select · Enter to view · x to stop · Esc to close"
    );
    assert_eq!(rows, [(5, 0), (6, 1), (9, 2)]);
    // x 停止焦点项；enter 查看代理（关掉面板）。
    assert_eq!(
        app.on_key(key(KeyCode::Char('x')), now),
        Some(Action::Manage(Manage::StopTask {
            id: "b1".into(),
            label: "npm test".into(),
        }))
    );
    app.on_key(key(KeyCode::Down), now);
    app.on_key(key(KeyCode::Down), now);
    assert_eq!(
        app.on_key(key(KeyCode::Enter), now),
        Some(Action::Panel(Panel::ViewAgent(Some(AgentId::new(
            "agent_a"
        )))))
    );
    assert!(app.overlay.is_none());
    // 都没有时：No tasks currently running。
    app.background.clear();
    app.agents = AgentList::default();
    app.open_background();
    let text = plain(&app.background_lines(80, 30, now).0);
    assert_eq!(text[2], "  No tasks currently running");
}

#[test]
fn the_shell_detail_keeps_the_latest_read_only() {
    let mut app = app();
    let now = Instant::now();
    app.background = vec![shell("b1", "npm test")];
    app.open_background();
    assert_eq!(
        app.on_key(key(KeyCode::Enter), now),
        Some(Action::Panel(Panel::ReadShell("b1".into())))
    );
    app.shell_reading("b1", 7);
    // 旧序号的回复丢弃。
    assert!(!app.on_shell_detail(
        "b1",
        6,
        Some(detail(BackgroundStatus::Running, &["old"], 1))
    ));
    let lines: Vec<String> = (1..=10).map(|n| format!("line {n}")).collect();
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    assert!(app.on_shell_detail("b1", 7, Some(detail(BackgroundStatus::Running, &refs, 42))));
    let text = plain(&app.background_lines(80, 30, now).0);
    assert_eq!(text[1], "  Shell details");
    assert_eq!(text[3], "  Status: running");
    assert_eq!(text[4], "  Runtime: 1m 23s");
    assert_eq!(text[5], "  Command: npm test");
    assert_eq!(text[7], "  Output:");
    assert!(text[8].starts_with("  ╭─"), "{text:?}");
    assert!(text[9].starts_with("  │ line 1 "), "{text:?}");
    assert_eq!(text[20], "  Showing 10 of 42 lines");
    assert_eq!(
        text.last().unwrap(),
        "  ← to go back · x to stop · Esc to close"
    );
    // 内核不再认识它：不再读；状态取通知的终态，没有时是 stopped。
    assert!(!app.on_shell_detail("b1", 7, None));
    let text = plain(&app.background_lines(80, 30, now).0);
    assert_eq!(text[3], "  Status: stopped");
    assert_eq!(text.last().unwrap(), "  ← to go back · Esc to close");
    // ← 回到列表。
    app.on_key(key(KeyCode::Left), now);
    let text = plain(&app.background_lines(80, 30, now).0);
    assert_eq!(text[1], "  Background");
}

#[test]
fn output_previews_cover_the_empty_large_and_missing_cases() {
    let mut app = app();
    let now = Instant::now();
    app.background = vec![shell("b1", "npm test")];
    app.open_background();
    app.on_key(key(KeyCode::Enter), now);
    app.shell_reading("b1", 1);
    let text = plain(&app.background_lines(80, 30, now).0);
    assert!(text.iter().any(|l| l.contains("loading...")), "{text:?}");
    let mut reply = detail(BackgroundStatus::Running, &[], 0);
    app.on_shell_detail("b1", 1, Some(reply.clone()));
    let text = plain(&app.background_lines(80, 30, now).0);
    assert!(text.iter().any(|l| l.contains("No output yet")), "{text:?}");
    reply.output = ShellOutput::TooLarge { bytes: 9 };
    app.on_shell_detail("b1", 1, Some(reply.clone()));
    let text = plain(&app.background_lines(80, 30, now).0);
    assert!(
        text.iter()
            .any(|l| l.contains("Output too large to preview")),
        "{text:?}"
    );
    reply.output = ShellOutput::Unavailable;
    reply.status = BackgroundStatus::Completed;
    assert!(!app.on_shell_detail("b1", 1, Some(reply)));
    let text = plain(&app.background_lines(80, 30, now).0);
    assert!(
        text.iter().any(|l| l.contains("Output unavailable")),
        "{text:?}"
    );
    assert_eq!(text[3], "  Status: completed");
}

#[test]
fn ansi_and_control_characters_are_stripped() {
    assert_eq!(plain_output("\u{1b}[31mred\u{1b}[0m\tx\u{7}"), "red    x");
}

/// 远端执行 R4（remote-exec.md §21.6）：远端的后台命令与缺省在远端的子代理在名称后面标 `@机器`（强调色），Shell 详情
/// 多一行机器。
#[test]
fn remote_entries_name_their_machine() {
    let mut app = app();
    let now = Instant::now();
    let mut remote = shell("b2", "make data");
    remote.machine = Some("dev-a".into());
    let mut helper = agent("agent_a");
    helper.machine = Some("gpu-1".into());
    app.background = vec![shell("b1", "npm test"), remote];
    app.agents = AgentList {
        entries: vec![helper],
        at: Some(now),
    };
    app.open_background();
    let (lines, _) = app.background_lines(80, 30, now);
    let text = plain(&lines);
    assert!(
        text.iter().any(|row| row.contains(" make data @dev-a ")),
        "{text:?}"
    );
    assert!(
        text.iter().any(|row| row.contains(" Review code @gpu-1 ")),
        "{text:?}"
    );
    assert!(
        text.iter()
            .any(|row| row.contains(" npm test ") && !row.contains('@')),
        "{text:?}"
    );
    let tag = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content == "@dev-a")
        .unwrap();
    assert_eq!(tag.style.fg, Some(app.palette.accent));
    // 焦点移到远端的命令，打开详情。
    app.on_key(key(KeyCode::Down), now);
    app.on_key(key(KeyCode::Enter), now);
    let text = plain(&app.background_lines(80, 30, now).0);
    assert_eq!(text[3], "  Machine: dev-a");
    assert!(text[4].starts_with("  Status: "), "{text:?}");
}

/// 远端执行 F96（remote-exec.md §23.3）：重连中的机器上的条目是那台机器上一份的内容，`@机器` 后面接 dim 的
/// `· reconnecting` / `· 重连中`。
#[test]
fn entries_of_a_reconnecting_machine_are_marked() {
    let mut app = app();
    let now = Instant::now();
    let mut stale = shell("b2", "make data");
    stale.machine = Some("dev-a".into());
    stale.reconnecting = true;
    app.background = vec![shell("b1", "npm test"), stale];
    app.open_background();
    let (lines, _) = app.background_lines(80, 30, now);
    let text = plain(&lines);
    assert!(
        text.iter()
            .any(|row| row.contains(" make data @dev-a · reconnecting ")),
        "{text:?}"
    );
    assert!(
        text.iter()
            .any(|row| row.contains(" npm test ") && !row.contains("reconnecting")),
        "{text:?}"
    );
    let note = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content == " · reconnecting")
        .unwrap();
    assert!(
        note.style
            .add_modifier
            .contains(ratatui::style::Modifier::DIM)
    );
    app.lang = Lang::Zh;
    let text = plain(&app.background_lines(80, 30, now).0);
    assert!(
        text.iter()
            .any(|row| row.contains(" make data @dev-a · 重连中 ")),
        "{text:?}"
    );
}

/// F98（remote-exec.md §25.4.5，Docker 试用 L9）：有缺省机器在远端的子代理时，分组名不写“本地”。
#[test]
fn the_agent_group_drops_local_for_an_agent_on_a_remote_machine() {
    for (lang, header) in [(Lang::En, "  Agents (1)"), (Lang::Zh, "  代理 (1)")] {
        let mut app = app();
        app.lang = lang;
        let now = Instant::now();
        app.background = vec![shell("b1", "npm test")];
        let mut remote = agent("agent_a");
        remote.machine = Some("dev-a".into());
        app.agents = AgentList {
            entries: vec![remote],
            at: Some(now),
        };
        app.open_background();
        let (lines, _) = app.background_lines(80, 30, now);
        let text = plain(&lines);
        assert!(text.iter().any(|line| line == header), "{text:#?}");
    }
}
