//! 子代理列表的树（docs/specs/rust-cli/tui-panels.md §1.2–§1.6，修复线 F30，聚焦视图）：各种查看位置
//! 下的可见集合与画面、`(+N)`、隐藏与驱逐后的挂靠、窗口滚动与顶格、`/tasks` 提示。画面按一棵固定的树核对
//! （main → A → A1 → A1a，A 另有 A2，main 另有 B）。
//!
//! 条目经 JSON 构造（`parked` 是 F30 加的字段；旧代码不认识它也能编译，用来确认这些测试在旧代码上失败）。
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Color;
use rust_cli_kernel_api::ids::AgentId;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::selection;
use serde_json::json;

use super::*;
use crate::ui::cells::BULLET;
use crate::ui::i18n::Lang;
use crate::ui::palette::{ColorLevel, Palette};

fn app_on(background: (u8, u8, u8)) -> App {
    App::new(
        Palette::new(ColorLevel::TrueColor, Some(background)),
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

fn app() -> App {
    app_on((0, 0, 0))
}

/// 一个条目：`owner` 是派生它的子代理；缺省在跑、后台、用时 5 秒。
fn entry(id: &str, owner: Option<&str>, description: &str) -> serde_json::Value {
    json!({
        "agent": id,
        "child": format!("sess_{id}"),
        "agent_type": "general-purpose",
        "description": description,
        "status": "running",
        "background": true,
        "elapsed_ms": 5_000,
        "version": 0,
        "owner": owner,
    })
}

fn ended(mut value: serde_json::Value, status: &str, ago_ms: u64) -> serde_json::Value {
    value["status"] = json!(status);
    value["ended_ago_ms"] = json!(ago_ms);
    value
}

fn set(app: &mut App, entries: Vec<serde_json::Value>, now: Instant) {
    app.agents = AgentList {
        entries: entries
            .into_iter()
            .map(|value| serde_json::from_value(value).unwrap())
            .collect(),
        at: Some(now),
    };
}

/// 快照顺序打乱（启动顺序：A、A1、B、A2、A1a），列表按有效父代理分组、深度优先展开。
fn family() -> Vec<serde_json::Value> {
    vec![
        entry("agent_a", None, "Alpha task"),
        entry("agent_a1", Some("agent_a"), "Alpha one"),
        entry("agent_b", None, "Beta task"),
        entry("agent_a2", Some("agent_a"), "Alpha two"),
        entry("agent_a1a", Some("agent_a1"), "Alpha one deep"),
    ]
}

fn view(app: &mut App, id: &str) {
    let found = app
        .agents
        .entries
        .iter()
        .find(|e| e.agent.as_str() == id)
        .cloned()
        .unwrap();
    app.viewing = Some(Viewing {
        agent: found.agent,
        kind: found.agent_type,
        description: found.description,
        version: found.version,
        status: found.status,
        messages: Vec::new(),
        owner: found.owner,
        view: Transcript::new(
            selection(),
            PermissionMode::Default,
            crate::view::test_workspace(),
        ),
    });
}

/// 每行去掉右侧的空白与用时（用时右对齐在行尾，这里只比左边的排版）。
fn rows(app: &App, width: usize, now: Instant) -> Vec<String> {
    plain(&app.agents_lines(width, now).0)
}

fn plain(lines: &[Line<'static>]) -> Vec<String> {
    lines
        .iter()
        .map(|line| {
            let text = crate::ui::text::plain(line);
            let text = text.trim_end();
            // 去掉行尾的用时（`5s`、`1m 5s`），留下左边的排版。
            match text.rfind("   ") {
                Some(at) if text[at..].trim().ends_with('s') => text[..at].trim_end().to_owned(),
                _ => text.to_owned(),
            }
        })
        .collect()
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn the_main_view_lists_only_top_level_agents_with_their_descendant_counts() {
    let mut app = app();
    let now = Instant::now();
    set(&mut app, family(), now);
    let (lines, targets) = app.agents_lines(100, now);
    assert_eq!(
        plain(&lines),
        [
            String::new(),
            format!("  {BULLET} main"),
            "  ◯ general-purpose (+3)  Alpha task".to_owned(),
            "  ◯ general-purpose       Beta task".to_owned(),
        ]
    );
    // 每一行仍是选择下标（0 = main）。
    assert_eq!(targets, [(1, 0), (2, 1), (3, 2)]);
    // 用时右对齐在行尾（F26 的用户决定不变）。
    let full = crate::ui::text::plain(&lines[2]);
    assert!(full.ends_with(" 5s"), "{full:?}");
    assert_eq!(full.width(), 100);
}

#[test]
fn viewing_a_top_level_agent_puts_its_children_under_it() {
    let mut app = app();
    let now = Instant::now();
    set(&mut app, family(), now);
    view(&mut app, "agent_a");
    assert_eq!(
        rows(&app, 100, now),
        [
            String::new(),
            "  ◯ main".to_owned(),
            format!("  {BULLET} general-purpose         Alpha task"),
            "  ├ ◯ general-purpose (+1)  Alpha one".to_owned(),
            "  └ ◯ general-purpose       Alpha two".to_owned(),
            "  ◯ general-purpose         Beta task".to_owned(),
        ]
    );
}

#[test]
fn viewing_a_nested_agent_shows_its_ancestors_siblings_and_children_only() {
    let mut app = app();
    let now = Instant::now();
    set(&mut app, family(), now);
    view(&mut app, "agent_a1");
    // B 是别的顶层分支：隐藏；链上的 A 不写 `(+N)`。
    assert_eq!(
        rows(&app, 100, now),
        [
            String::new(),
            "  ◯ main".to_owned(),
            "  ◯ general-purpose      Alpha task".to_owned(),
            format!("  ├ {BULLET} general-purpose    Alpha one"),
            "    └ ◯ general-purpose  Alpha one deep".to_owned(),
            "  └ ◯ general-purpose    Alpha two".to_owned(),
        ]
    );
    // 看最深的 A1a：只剩祖先链（A2、B 隐藏），A1 后面没有同一父代理的可见行，画 `└`。
    view(&mut app, "agent_a1a");
    assert_eq!(
        rows(&app, 100, now),
        [
            String::new(),
            "  ◯ main".to_owned(),
            "  ◯ general-purpose      Alpha task".to_owned(),
            "  └ ◯ general-purpose    Alpha one".to_owned(),
            format!("    └ {BULLET} general-purpose  Alpha one deep"),
        ]
    );
}

#[test]
fn completed_background_agents_hide_and_their_children_move_up_then_to_the_top() {
    let mut app = app();
    let now = Instant::now();
    // A2 完成（后台）：立即隐藏，不算进 A 的 `(+N)`。
    let mut entries = family();
    entries[3] = ended(entries[3].clone(), "completed", 1_000);
    set(&mut app, entries.clone(), now);
    assert_eq!(
        rows(&app, 100, now)[2],
        "  ◯ general-purpose (+2)  Alpha task"
    );
    view(&mut app, "agent_a1");
    assert_eq!(
        rows(&app, 100, now),
        [
            String::new(),
            "  ◯ main".to_owned(),
            "  ◯ general-purpose      Alpha task".to_owned(),
            format!("  └ {BULLET} general-purpose    Alpha one"),
            "    └ ◯ general-purpose  Alpha one deep".to_owned(),
        ]
    );
    // A1 也完成了（后台、没停放）：隐藏，A1a 挂到祖父 A 下。
    entries[1] = ended(entries[1].clone(), "completed", 2_000);
    set(&mut app, entries, now);
    view(&mut app, "agent_a1a");
    assert_eq!(
        rows(&app, 100, now),
        [
            String::new(),
            "  ◯ main".to_owned(),
            "  ◯ general-purpose    Alpha task".to_owned(),
            format!("  └ {BULLET} general-purpose  Alpha one deep"),
        ]
    );
    // A1 结束 30 秒后被驱逐：A1a 提升为顶层（不挂到 A 下），兄弟是全部顶层代理，B 又出现（顶层按启动顺序：A、B、A1a）。
    let later = now + Duration::from_secs(29);
    assert_eq!(
        rows(&app, 100, later),
        [
            String::new(),
            "  ◯ main".to_owned(),
            "  ◯ general-purpose  Alpha task".to_owned(),
            "  ◯ general-purpose  Beta task".to_owned(),
            format!("  {BULLET} general-purpose  Alpha one deep"),
        ]
    );
}

#[test]
fn foreground_failed_and_parked_agents_stay_in_the_tree() {
    let mut app = app();
    let now = Instant::now();
    let mut entries = family();
    // 前台完成的 A2 照常留 30 秒（绿色）；失败的后台 B 留 30 秒（红色）。
    entries[3] = ended(entries[3].clone(), "completed", 1_000);
    entries[3]["background"] = json!(false);
    entries[2] = ended(entries[2].clone(), "failed", 1_000);
    // A1 停放了 60 秒（还有在跑的 A1a）：不驱逐，照常当 A1a 的父代理，圆点不上色。
    entries[1] = ended(entries[1].clone(), "completed", 60_000);
    entries[1]["parked"] = json!(true);
    set(&mut app, entries, now);
    view(&mut app, "agent_a");
    let (lines, _) = app.agents_lines(100, now);
    assert_eq!(
        plain(&lines),
        [
            String::new(),
            "  ◯ main".to_owned(),
            format!("  {BULLET} general-purpose         Alpha task"),
            "  ├ ◯ general-purpose (+1)  Alpha one".to_owned(),
            "  └ ◯ general-purpose       Alpha two".to_owned(),
            "  ◯ general-purpose         Beta task".to_owned(),
        ]
    );
    let dot = |line: &Line<'static>| {
        line.spans
            .iter()
            .find(|span| span.content == "◯" || span.content == BULLET)
            .and_then(|span| span.style.fg)
    };
    assert_eq!(dot(&lines[3]), None, "parked: no color");
    assert_eq!(dot(&lines[4]), Some(Color::Green));
    assert_eq!(dot(&lines[5]), Some(Color::Red));
    // `(+N)` 与没选中、没查看的行的连接符都是 dim。
    let span = |line: &Line<'static>, text: &str| {
        line.spans
            .iter()
            .find(|span| span.content.contains(text))
            .map(|span| span.style)
            .unwrap()
    };
    assert!(span(&lines[3], "(+1)").add_modifier.contains(Modifier::DIM));
    assert!(span(&lines[3], "├").add_modifier.contains(Modifier::DIM));
}

#[test]
fn the_window_follows_the_focus_and_rows_under_a_scrolled_out_parent_start_at_the_left() {
    let mut app = app();
    let now = Instant::now();
    let mut entries = vec![entry("agent_a", None, "Alpha task")];
    for i in 1..=6 {
        entries.push(entry(
            &format!("agent_c{i}"),
            Some("agent_a"),
            &format!("Child {i}"),
        ));
    }
    set(&mut app, entries, now);
    view(&mut app, "agent_a");
    // 焦点在 C6（第 7 行）：窗口起点 2，A 滚出窗口，C2–C6 顶格；名字列宽仍按深度 1 算，描述列不动。
    app.focus = Some(Focus::Agents(7));
    let text = rows(&app, 100, now);
    assert_eq!(text.len(), 1 + 1 + 5 + 1, "{text:?}");
    assert!(text[1].starts_with("  ◯ main"), "{text:?}");
    assert!(text[1].ends_with("↑ 2 more"), "{text:?}");
    assert_eq!(text[2], "  ◯ general-purpose    Child 2");
    assert_eq!(text[6], "› ◯ general-purpose    Child 6");
    // 行数超过 5、下面没有更多：最后是空行（写一个空格，高度不跳）。
    assert_eq!(text[7].trim(), "");
    // 焦点回到 C1：A 在窗口里，孩子画连接符；下面还有 2 行。
    app.focus = Some(Focus::Agents(2));
    let text = rows(&app, 100, now);
    assert_eq!(text[1], "  ◯ main");
    assert_eq!(text[2], format!("  {BULLET} general-purpose    Alpha task"));
    assert_eq!(text[3], "› ├ ◯ general-purpose  Child 1");
    assert_eq!(text[6], "  ├ ◯ general-purpose  Child 4");
    assert_eq!(text[7], "  ↓ 2 more");
    // 不在列表里时窗口跟着正在查看的那一行：看 C6 时同样从 C2 起。
    app.focus = None;
    view(&mut app, "agent_c6");
    let text = rows(&app, 100, now);
    assert!(text[1].ends_with("↑ 2 more"), "{text:?}");
    assert_eq!(text[6], format!("  {BULLET} general-purpose    Child 6"));
}

#[test]
fn the_name_column_fits_the_deepest_row_and_stops_at_28_columns() {
    let mut app = app();
    let now = Instant::now();
    let mut a = entry("agent_a", None, "Alpha task");
    a["agent_type"] = json!("Explore");
    let mut a1 = entry("agent_a1", Some("agent_a"), "Alpha one");
    a1["agent_type"] = json!("a-really-long-custom-agent-name");
    set(&mut app, vec![a, a1], now);
    view(&mut app, "agent_a");
    let text = rows(&app, 100, now);
    // 2 × 1 + 31 > 28：列宽 28，孩子的名字格 26，名字截断。
    assert_eq!(text[2], format!("  {BULLET} {:<28}  Alpha task", "Explore"));
    assert_eq!(text[3], "  └ ◯ a-really-long-custom-agen…  Alpha one");
    // 描述列对齐（按显示宽度比，圆点与连接符是多字节字符）。
    let column = |row: &str| row[..row.find("Alpha").unwrap()].width();
    assert_eq!(column(&text[2]), column(&text[3]));
}

#[test]
fn keys_walk_the_tree_in_order() {
    let mut app = app();
    let now = Instant::now();
    set(&mut app, family(), now);
    view(&mut app, "agent_a1");
    // ↓ 进入列表（main），再往下按树的顺序：A、A1、A1a、A2。
    app.on_key(key(KeyCode::Down), now);
    assert_eq!(app.focus, Some(Focus::Agents(0)));
    for _ in 0..3 {
        app.on_key(key(KeyCode::Down), now);
    }
    assert_eq!(
        app.on_key(key(KeyCode::Char('x')), now),
        Some(Action::Manage(crate::ui::manage::Manage::StopTask {
            id: "agent_a1a".into(),
            label: "Alpha one deep".into(),
        }))
    );
    app.on_key(key(KeyCode::Down), now);
    app.on_key(key(KeyCode::Down), now);
    assert_eq!(
        app.on_key(key(KeyCode::Enter), now),
        Some(Action::Panel(Panel::ViewAgent(Some(AgentId::new(
            "agent_a2"
        )))))
    );
}

#[test]
fn the_footer_points_to_tasks_while_completed_background_agents_are_hidden() {
    let mut app = app();
    let now = Instant::now();
    let mut entries = family();
    entries[2] = ended(entries[2].clone(), "completed", 1_000);
    set(&mut app, entries, now);
    let footer = |app: &App| crate::ui::text::plain(&crate::ui::footer::footer(app, 120).0);
    assert!(
        footer(&app).contains("· /tasks to see subagents"),
        "{}",
        footer(&app)
    );
    // 放不下时排在 `? for shortcuts` 之后、右侧的统计之前省掉（tui-display.md §2.6）。
    let at = |app: &App, width| crate::ui::text::plain(&crate::ui::footer::footer(app, width).0);
    assert!(
        at(&app, 120).contains("? for shortcuts"),
        "{}",
        at(&app, 120)
    );
    // 权限模式后面没有切换提示了（2026-10-20），左侧短了 21 列：阈值随之左移。
    let fits = at(&app, 79);
    assert!(
        fits.contains("/tasks to see subagents") && !fits.contains("? for shortcuts"),
        "{fits}"
    );
    let narrow = at(&app, 69);
    assert!(
        !narrow.contains("/tasks") && narrow.contains("← for projects"),
        "{narrow}"
    );
    app.lang = Lang::Zh;
    assert!(
        footer(&app).contains("· /tasks 查看子代理"),
        "{}",
        footer(&app)
    );
    // 正在查看它（在查看链上）：它照常成行，不提示。
    app.lang = Lang::En;
    view(&mut app, "agent_b");
    assert!(!footer(&app).contains("/tasks"), "{}", footer(&app));
    // 看顶层的 B：全部顶层代理，A 不在查看链上，照常写 `(+3)`，名字列按它算。
    assert_eq!(
        rows(&app, 100, now)[2..],
        [
            "  ◯ general-purpose (+3)  Alpha task".to_owned(),
            format!("  {BULLET} {:<20}  Beta task", "general-purpose"),
        ]
    );
    // 结束 30 秒后被驱逐：提示消失。
    app.viewing = None;
    set(
        &mut app,
        vec![ended(
            entry("agent_b", None, "Beta task"),
            "completed",
            31_000,
        )],
        now,
    );
    assert!(!footer(&app).contains("/tasks"), "{}", footer(&app));
}

#[test]
fn chinese_and_both_themes() {
    for background in [(0, 0, 0), (255, 255, 255)] {
        let mut app = app_on(background);
        app.lang = Lang::Zh;
        let now = Instant::now();
        let mut entries = vec![entry("agent_a", None, "Alpha task")];
        for i in 1..=6 {
            entries.push(entry(
                &format!("agent_c{i}"),
                Some("agent_a"),
                &format!("Child {i}"),
            ));
        }
        set(&mut app, entries, now);
        view(&mut app, "agent_a");
        app.focus = Some(Focus::Agents(7));
        let (lines, _) = app.agents_lines(100, now);
        let text = plain(&lines);
        assert!(text[1].starts_with("  ◯ 主会话"), "{text:?}");
        assert!(text[1].ends_with("↑ 还有 2 个"), "{text:?}");
        // 选中的行按选择列表组件画：整行底色条（深浅两种背景各一种底色）。
        let fill = if background == (0, 0, 0) {
            Color::Rgb(99, 168, 248)
        } else {
            Color::Rgb(164, 205, 251)
        };
        assert_eq!(lines[6].style.bg, Some(fill), "{background:?}");
        app.focus = Some(Focus::Agents(2));
        let text = plain(&app.agents_lines(100, now).0);
        assert_eq!(text[7], "  ↓ 还有 2 个");
    }
}

/// 查看对象不在了（tui-panels.md §1.6）：被查看的子代理不在快照里了 → 改看它的 owner（原始的父代理）；owner 也不在或已驱逐 →
/// 回到主会话；还在快照里时不动。
#[test]
fn a_viewed_agent_that_leaves_the_snapshot_hands_the_view_to_its_owner() {
    let mut app = app();
    let now = Instant::now();
    set(&mut app, family(), now);
    view(&mut app, "agent_a1a");
    assert_eq!(app.viewed_gone(now), None);
    // A1a 不在了：A1 还在跑，改看 A1。
    let mut entries = family();
    entries.remove(4);
    set(&mut app, entries.clone(), now);
    assert_eq!(app.viewed_gone(now), Some(Some(AgentId::new("agent_a1"))));
    // A1 已结束 30 秒以上（驱逐）：回到主会话。
    entries[1] = ended(entries[1].clone(), "completed", 31_000);
    set(&mut app, entries, now);
    assert_eq!(app.viewed_gone(now), Some(None));
    // 主线程派生的（没有 owner）不在了：回到主会话。
    set(&mut app, family(), now);
    view(&mut app, "agent_b");
    set(&mut app, vec![entry("agent_a", None, "Alpha task")], now);
    assert_eq!(app.viewed_gone(now), Some(None));
    // 没在查看时不管。
    app.viewing = None;
    assert_eq!(app.viewed_gone(now), None);
}
