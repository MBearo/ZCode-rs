//! 子代理列表（tui-panels.md §1）：可见规则、排版与按键。
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_kernel_api::ids::{AgentId, SessionId};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::selection;

use super::*;
use crate::ui::i18n::Lang;
use crate::ui::manage::Manage;
use crate::ui::palette::{ColorLevel, Palette};

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

fn entry(id: &str, status: SubagentStatus, ended_ago_ms: Option<u64>) -> SubagentEntry {
    SubagentEntry {
        agent: AgentId::new(id),
        child: SessionId::new(format!("sess_subagent_{id}")),
        agent_type: "Explore".into(),
        description: format!("Look at {id}"),
        status,
        background: true,
        waiting: false,
        elapsed_ms: 65_000,
        ended_ago_ms,
        version: 0,
        owner: None,
        activity: None,
        parked: false,
        machine: None,
    }
}

fn plain(lines: &[Line<'static>]) -> Vec<String> {
    lines.iter().map(crate::ui::text::plain).collect()
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn ended_agents_leave_the_list_thirty_seconds_after_they_end() {
    let mut app = app();
    let now = Instant::now();
    app.agents = AgentList {
        entries: vec![
            entry("agent_a", SubagentStatus::Running, None),
            entry("agent_b", SubagentStatus::Completed, Some(29_000)),
            entry("agent_c", SubagentStatus::Failed, Some(31_000)),
        ],
        at: Some(now),
    };
    let ids = |app: &App, at| {
        app.agent_rows(at)
            .iter()
            .map(|e| e.agent.to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&app, now), ["agent_a", "agent_b"]);
    // 快照到达后又过了 2 秒：agent_b 也到了 30 秒。
    assert_eq!(ids(&app, now + Duration::from_secs(2)), ["agent_a"]);
    // 在跑的行的用时往后推，结束的不变。
    let later = now + Duration::from_secs(5);
    assert_eq!(
        elapsed_ms(&app.agents, &app.agents.entries[0], later),
        70_000
    );
    assert_eq!(
        elapsed_ms(&app.agents, &app.agents.entries[1], later),
        65_000
    );
}

#[test]
fn the_list_has_main_then_one_line_per_agent() {
    let mut app = app();
    let now = Instant::now();
    let mut waiting = entry("agent_a", SubagentStatus::Running, None);
    waiting.waiting = true;
    waiting.background = false;
    // 前台完成的照常留 30 秒；后台完成的立即隐藏（修复线 F30，tui-panels.md §1.2，见 agents_tree_tests.rs）。
    let mut done = entry("agent_b", SubagentStatus::Completed, Some(0));
    done.background = false;
    app.agents = AgentList {
        entries: vec![waiting, done],
        at: Some(now),
    };
    let (lines, targets) = app.agents_lines(80, now);
    let text = plain(&lines);
    assert_eq!(text[0], "");
    assert!(text[1].ends_with(" main"), "{text:?}");
    assert!(
        text[2].starts_with("  ◯ Explore  Look at agent_a"),
        "{text:?}"
    );
    assert!(text[2].contains("Look at agent_a · waiting "), "{text:?}");
    assert!(text[2].ends_with(" 1m 5s"), "{text:?}");
    assert!(text[3].ends_with("1m 5s"), "{text:?}");
    assert_eq!(targets, [(1, 0), (2, 1), (3, 2)]);
    // 完成的圆点是绿色（F30 起圆点前面多一段连接符，顶层的行为空）。
    let dot = lines[3]
        .spans
        .iter()
        .find(|span| span.content == "◯")
        .unwrap();
    assert_eq!(dot.style.fg, Some(Color::Green));
    // 没有子代理时不显示。
    app.agents = AgentList::default();
    assert!(app.agents_lines(80, now).0.is_empty());
}

#[test]
fn more_than_five_agents_end_with_a_count() {
    let mut app = app();
    let now = Instant::now();
    app.agents = AgentList {
        entries: (0..7)
            .map(|i| entry(&format!("agent_{i}"), SubagentStatus::Running, None))
            .collect(),
        at: Some(now),
    };
    let text = plain(&app.agents_lines(80, now).0);
    assert_eq!(text.len(), 1 + 1 + 5 + 1);
    assert_eq!(text.last().unwrap(), "  ↓ 2 more");
}

#[test]
fn keys_move_view_stop_and_leave() {
    let mut app = app();
    let now = Instant::now();
    let mut foreground = entry("agent_f", SubagentStatus::Running, None);
    foreground.background = false;
    app.agents = AgentList {
        entries: vec![entry("agent_b", SubagentStatus::Running, None), foreground],
        at: Some(now),
    };
    // 输入框为空时 ↓ 进入列表（选中 main）。
    assert_eq!(app.on_key(key(KeyCode::Down), now), None);
    assert_eq!(app.focus, Some(Focus::Agents(0)));
    assert_eq!(
        app.agents_focus_hint(now),
        Some("{↑/↓} to select · {enter} to view · {esc} to close")
    );
    app.on_key(key(KeyCode::Down), now);
    assert_eq!(
        app.agents_focus_hint(now),
        Some("{↑/↓} to select · {enter} to view · {x} to stop · {esc} to close")
    );
    // x 停止后台子代理：与 `/tasks` 同一条路径。
    assert_eq!(
        app.on_key(key(KeyCode::Char('x')), now),
        Some(Action::Manage(Manage::StopTask {
            id: "agent_b".into(),
            label: "Look at agent_b".into(),
        }))
    );
    // 前台子代理不能停。
    app.on_key(key(KeyCode::Down), now);
    assert_eq!(app.on_key(key(KeyCode::Char('x')), now), None);
    assert_eq!(
        app.on_key(key(KeyCode::Enter), now),
        Some(Action::Panel(Panel::ViewAgent(Some(AgentId::new(
            "agent_f"
        )))))
    );
    assert_eq!(app.focus, None);
    // 在 main 上按 ↑ 回到输入框；esc 离开列表。
    app.on_key(key(KeyCode::Down), now);
    app.on_key(key(KeyCode::Up), now);
    assert_eq!(app.focus, None);
    app.on_key(key(KeyCode::Down), now);
    app.on_key(key(KeyCode::Esc), now);
    assert_eq!(app.focus, None);
}

#[test]
fn viewing_is_read_only_and_esc_returns() {
    let mut app = app();
    let now = Instant::now();
    app.agents = AgentList {
        entries: vec![entry("agent_a", SubagentStatus::Running, None)],
        at: Some(now),
    };
    app.viewing = Some(Viewing {
        agent: AgentId::new("agent_a"),
        kind: "Explore".into(),
        description: "Look at agent_a".into(),
        version: 0,
        status: SubagentStatus::Running,
        messages: Vec::new(),
        owner: None,
        view: Transcript::new(
            selection(),
            PermissionMode::Default,
            crate::view::test_workspace(),
        ),
    });
    app.on_key(key(KeyCode::Char('h')), now);
    assert!(app.composer.is_empty(), "typing is ignored while viewing");
    assert_eq!(app.on_paste("pasted"), None);
    assert!(app.composer.is_empty());
    let header = plain(&app.viewing_header().unwrap());
    assert_eq!(header[1], "  ↳ Explore · Look at agent_a");
    assert_eq!(
        app.on_key(key(KeyCode::Esc), now),
        Some(Action::Panel(Panel::ViewAgent(None)))
    );
}

/// L35（stall-visibility.md §4.3）：描述后面接当前活动，用时右对齐在行尾；重试与停顿用 error 色，倒计时按快照
/// 到达后往后推。修复线 F26：没有“思考中”（kernel-api 的 `Thinking` 不显示）。修复线 F62：没有工具名（kernel-api 的
/// `Tool` 不显示；用户截图里 `Explore 架构分层与进程模型` 后面闪出 ` · Bash`）。
#[test]
fn rows_show_what_the_agent_is_doing() {
    let mut app = app();
    let now = Instant::now();
    let with = |id: &str, activity: SubagentActivity| {
        let mut row = entry(id, SubagentStatus::Running, None);
        row.activity = Some(activity);
        row
    };
    app.agents = AgentList {
        entries: vec![
            with("agent_a", SubagentActivity::Thinking),
            with(
                "agent_b",
                SubagentActivity::Tool {
                    name: "Bash".into(),
                },
            ),
            with(
                "agent_c",
                SubagentActivity::Retrying {
                    attempt: 2,
                    max_retries: 10,
                    retry_in_ms: 9_000,
                    error: "server error 500: boom".into(),
                },
            ),
            with(
                "agent_d",
                SubagentActivity::Stalled {
                    retry_in_ms: 200_000,
                },
            ),
        ],
        at: Some(now),
    };
    let later = now + Duration::from_secs(3);
    let (lines, _) = app.agents_lines(100, later);
    let text = plain(&lines);
    assert!(text[2].contains("Look at agent_a "), "{text:?}");
    assert!(!text[2].contains("Thinking"), "{text:?}");
    assert!(text[3].contains("Look at agent_b "), "{text:?}");
    assert!(!text[3].contains("Bash"), "{text:?}");
    assert!(!text[3].contains(" · "), "{text:?}");
    assert!(
        text[4].contains("Look at agent_c · Retrying in 6s · attempt 2/10 "),
        "{text:?}"
    );
    assert!(
        text[5].contains("Look at agent_d · Waiting for API response "),
        "{text:?}"
    );
    for row in &text[2..6] {
        assert!(row.ends_with(" 1m 8s"), "{text:?}");
    }
    let doing = |line: &Line<'static>| {
        line.spans
            .iter()
            .find(|span| span.content.starts_with(" · "))
            .map(|span| span.style.fg)
    };
    assert_eq!(doing(&lines[4]), Some(Some(Color::Red)));
    assert_eq!(doing(&lines[3]), None);
    // 中文。
    app.lang = Lang::Zh;
    let text = plain(&app.agents_lines(100, later).0);
    assert!(!text[2].contains("思考中"), "{text:?}");
    assert!(text[4].contains(" · 6s后重试 · 第 2/10 次 "), "{text:?}");
    assert!(text[5].contains(" · 等待 API 响应 "), "{text:?}");
    // 等用户回答时只写 waiting。
    app.lang = Lang::En;
    app.agents.entries[0].waiting = true;
    let text = plain(&app.agents_lines(100, later).0);
    assert!(text[2].contains("Look at agent_a · waiting "), "{text:?}");
    assert!(text[2].ends_with(" 1m 8s"), "{text:?}");
}

/// 缺陷修复（修复线 F26）：活动变化时右侧的用时不左右跳。原来用时排在活动前面、描述按两者合计截断，活动从
/// `∴ Thinking` 换成 `Bash` 时用时往右移了 6 列（用户截图里的 `13s` / `12s`）。
#[test]
fn the_duration_stays_in_place_when_the_activity_changes() {
    let mut app = app();
    let now = Instant::now();
    let mut row = entry("agent_a", SubagentStatus::Running, None);
    row.elapsed_ms = 13_000;
    let column = |app: &App| {
        let text = plain(&app.agents_lines(80, now).0);
        let line = text[2].clone();
        let at = line.find("13s").expect("the duration");
        (line[..at].width(), line.width())
    };
    let mut seen = Vec::new();
    for activity in [
        None,
        Some(SubagentActivity::Thinking),
        Some(SubagentActivity::Tool {
            name: "Bash".into(),
        }),
        Some(SubagentActivity::Tool {
            name: "mcp__github__search_repositories".into(),
        }),
        Some(SubagentActivity::Stalled {
            retry_in_ms: 200_000,
        }),
    ] {
        row.activity = activity;
        app.agents = AgentList {
            entries: vec![row.clone()],
            at: Some(now),
        };
        seen.push(column(&app));
    }
    assert!(seen.iter().all(|c| *c == seen[0]), "{seen:?}");
    assert_eq!(seen[0].1, 80, "the row fills the width");
    // 很窄时活动与描述让位，用时仍在行尾。
    row.activity = Some(SubagentActivity::Tool {
        name: "a_tool_with_a_very_long_name_indeed".into(),
    });
    app.agents = AgentList {
        entries: vec![row],
        at: Some(now),
    };
    let text = plain(&app.agents_lines(40, now).0);
    assert!(text[2].ends_with(" 13s"), "{text:?}");
    assert!(text[2].width() <= 40, "{text:?}");
}

/// 远端执行 R4（remote-exec.md §21.6）：子代理的缺省机器在远端时，类型后面标 ` @机器`（强调色），描述列照旧对齐。
#[test]
fn agents_on_a_remote_machine_name_it() {
    let mut app = app();
    let now = Instant::now();
    let mut remote = entry("agent_a", SubagentStatus::Running, None);
    remote.machine = Some("dev-a".into());
    let local = entry("agent_b", SubagentStatus::Running, None);
    app.agents = AgentList {
        entries: vec![remote, local],
        at: Some(now),
    };
    let (lines, _) = app.agents_lines(80, now);
    let text = plain(&lines);
    assert!(
        text[2].starts_with("  ◯ Explore @dev-a  Look at agent_a"),
        "{text:?}"
    );
    assert!(
        text[3].starts_with("  ◯ Explore         Look at agent_b"),
        "{text:?}"
    );
    let tag = lines[2]
        .spans
        .iter()
        .find(|span| span.content == " @dev-a")
        .unwrap();
    assert_eq!(tag.style.fg, Some(app.palette.accent));
}
