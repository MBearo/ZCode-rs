//! 子代理列表的树（docs/specs/rust-cli/tui-panels.md §1.2–§1.6，修复线 F30，聚焦视图）：各种查看位置
//! 下的可见集合与画面、`(+N)`、驱逐后的挂靠、窗口滚动与顶格。画面按一棵固定的树核对
//! （main → A → A1 → A1a，A 另有 A2，main 另有 B）。
//!
//! 条目经 JSON 构造（`parked` 是 F30 加的字段；旧代码不认识它也能编译，用来确认这些测试在旧代码上失败）。
use std::time::Instant;

use crossterm::event::KeyCode;
use rust_cli_kernel_api::ids::AgentId;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::selection;
use rust_cli_tui_kit::render::width::string_width;
use serde_json::json;

use super::*;
use crate::ui::i18n::Lang;
use crate::ui::probe::{ColorLevel, Colors};
use crate::ui::test_support::{ctrl, key};

/// 真彩终端；前景取与背景相反的黑白，报告了两者，是 rich 的主题（tui-kit.md §3.4）。
fn app_on(background: (u8, u8, u8)) -> App {
    let foreground = if background == (0, 0, 0) {
        (230, 230, 230)
    } else {
        (0, 0, 0)
    };
    crate::ui::test_support::app_with(
        Colors::new(ColorLevel::TrueColor, Some(background)).with_foreground(Some(foreground)),
        Lang::En,
        PermissionMode::Default,
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

fn set(app: &mut App, entries: Vec<serde_json::Value>) {
    app.snapshots.agents = AgentList {
        entries: entries
            .into_iter()
            .map(|value| serde_json::from_value(value).unwrap())
            .collect(),
        at: Some(T0),
        shown: T0,
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
        .snapshots
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
fn rows(app: &mut App, width: usize, shown: u64) -> Vec<String> {
    plain(&layer(app, width, shown).0)
}

/// 快照到达的墙上时间（Unix 毫秒）。
const T0: u64 = 1_800_000_000_000;

/// 任务卡的 Subagents：排好的行与（行，选择下标）；卡开着时按它选中的行，否则选中 main。
/// `shown` 是这一帧的墙上时间（`draw::draw` 写进 `AgentList::shown`）。
fn layer(app: &mut App, width: usize, shown: u64) -> (Vec<StyledLine>, Vec<(usize, usize)>) {
    app.snapshots.agents.shown = shown;
    let selected = app.task_card().map_or(0, |card| app.agent_row(card));
    app.subagent_rows(selected, width)
}

/// 打开任务卡，选中 Subagents 的第 `index` 项（0 = main）。
fn select(app: &mut App, index: usize) {
    if app.task_card().is_none() {
        app.open_tasks();
    }
    let agent = app.agent_at(index);
    app.task_card_mut().unwrap().agent = agent;
}

fn plain(lines: &[StyledLine]) -> Vec<String> {
    lines
        .iter()
        .map(|line| {
            let text = line.plain();
            let text = text.trim_end();
            // 去掉行尾的用时（`5s`、`1m 5s`），留下左边的排版。
            match text.rfind("   ") {
                Some(at) if text[at..].trim().ends_with('s') => text[..at].trim_end().to_owned(),
                _ => text.to_owned(),
            }
        })
        .collect()
}

#[test]
fn the_main_view_lists_only_top_level_agents_with_their_descendant_counts() {
    let mut app = app();
    set(&mut app, family());
    let (lines, targets) = layer(&mut app, 100, T0);
    assert_eq!(
        plain(&lines),
        [
            // 主会话也在名字列里；正在看的写 fg3 的 `current`（Puffin `/resume` 的列表）。
            format!("     {:<20}  current", "main"),
            "   ○ general-purpose (+3)  Alpha task".to_owned(),
            "   ○ general-purpose       Beta task".to_owned(),
        ]
    );
    // 每一行仍是选择下标（0 = main）。
    assert_eq!(targets, [(0, 0), (1, 1), (2, 2)]);
    // 用时右对齐在行尾（F26 的用户决定不变），行尾是量度（宽 − 2，Puffin 子代理的时间列）。
    let full = lines[1].plain();
    assert!(full.trim_end().ends_with(" 5s"), "{full:?}");
    assert_eq!(string_width(&full), 98);
}

#[test]
fn viewing_a_top_level_agent_puts_its_children_under_it() {
    let mut app = app();
    set(&mut app, family());
    view(&mut app, "agent_a");
    assert_eq!(
        rows(&mut app, 100, T0),
        [
            "     main".to_owned(),
            "   ○ general-purpose         Alpha task  current".to_owned(),
            "   ○ ├ general-purpose (+1)  Alpha one".to_owned(),
            "   ○ └ general-purpose       Alpha two".to_owned(),
            "   ○ general-purpose         Beta task".to_owned(),
        ]
    );
}

#[test]
fn viewing_a_nested_agent_shows_its_ancestors_siblings_and_children_only() {
    let mut app = app();
    set(&mut app, family());
    view(&mut app, "agent_a1");
    // B 是别的顶层分支：隐藏；链上的 A 不写 `(+N)`。
    assert_eq!(
        rows(&mut app, 100, T0),
        [
            "     main".to_owned(),
            "   ○ general-purpose      Alpha task".to_owned(),
            "   ○ ├ general-purpose    Alpha one  current".to_owned(),
            "   ○   └ general-purpose  Alpha one deep".to_owned(),
            "   ○ └ general-purpose    Alpha two".to_owned(),
        ]
    );
    // 看最深的 A1a：只剩祖先链（A2、B 隐藏），A1 后面没有同一父代理的可见行，画 `└`。
    view(&mut app, "agent_a1a");
    assert_eq!(
        rows(&mut app, 100, T0),
        [
            "     main".to_owned(),
            "   ○ general-purpose      Alpha task".to_owned(),
            "   ○ └ general-purpose    Alpha one".to_owned(),
            "   ○   └ general-purpose  Alpha one deep  current".to_owned(),
        ]
    );
}

/// 完成的后台代理不再藏起（tui-panels.md §1.2，2026-10-31：Background 面板删掉后任务卡是看它们的唯一入口）：`✓` 一行留到
/// 驱逐，照常当父代理、算进 `(+N)`；驱逐后孩子提升为顶层。
#[test]
fn completed_background_agents_stay_until_evicted_then_their_children_move_to_the_top() {
    let mut app = app();
    let mut entries = family();
    entries[3] = ended(entries[3].clone(), "completed", 1_000);
    set(&mut app, entries.clone());
    assert_eq!(
        rows(&mut app, 100, T0)[1],
        "   ○ general-purpose (+3)  Alpha task"
    );
    view(&mut app, "agent_a1");
    assert_eq!(
        rows(&mut app, 100, T0),
        [
            "     main".to_owned(),
            "   ○ general-purpose      Alpha task".to_owned(),
            "   ○ ├ general-purpose    Alpha one  current".to_owned(),
            "   ○   └ general-purpose  Alpha one deep".to_owned(),
            "   ✓ └ general-purpose    Alpha two".to_owned(),
        ]
    );
    // A1 也完成了（后台、没停放）：照常是 A1a 的父代理。
    entries[1] = ended(entries[1].clone(), "completed", 2_000);
    set(&mut app, entries);
    view(&mut app, "agent_a1a");
    assert_eq!(
        rows(&mut app, 100, T0),
        [
            "     main".to_owned(),
            "   ○ general-purpose      Alpha task".to_owned(),
            "   ✓ └ general-purpose    Alpha one".to_owned(),
            "   ○   └ general-purpose  Alpha one deep  current".to_owned(),
        ]
    );
    // A1 结束 30 秒后被驱逐：A1a 提升为顶层（不挂到 A 下），兄弟是全部顶层代理，B 又出现（顶层按启动顺序：A、B、A1a）。
    let later = T0 + 29_000;
    assert_eq!(
        rows(&mut app, 100, later),
        [
            "     main".to_owned(),
            "   ○ general-purpose  Alpha task".to_owned(),
            "   ○ general-purpose  Beta task".to_owned(),
            "   ○ general-purpose  Alpha one deep  current".to_owned(),
        ]
    );
}

#[test]
fn foreground_failed_and_parked_agents_stay_in_the_tree() {
    let mut app = app();
    let mut entries = family();
    // 前台完成的 A2 照常留 30 秒（绿色）；失败的后台 B 留 30 秒（红色）。
    entries[3] = ended(entries[3].clone(), "completed", 1_000);
    entries[3]["background"] = json!(false);
    entries[2] = ended(entries[2].clone(), "failed", 1_000);
    // A1 停放了 60 秒（还有在跑的 A1a）：不驱逐，照常当 A1a 的父代理，圆点不上色。
    entries[1] = ended(entries[1].clone(), "completed", 60_000);
    entries[1]["parked"] = json!(true);
    set(&mut app, entries);
    view(&mut app, "agent_a");
    let (lines, _) = layer(&mut app, 100, T0);
    assert_eq!(
        plain(&lines),
        [
            "     main".to_owned(),
            "   ○ general-purpose         Alpha task  current".to_owned(),
            "   ○ ├ general-purpose (+1)  Alpha one".to_owned(),
            "   ✓ └ general-purpose       Alpha two".to_owned(),
            "   ✗ general-purpose         Beta task".to_owned(),
        ]
    );
    let dot = |line: &StyledLine| line.spans[1].style;
    // Puffin 的记号：停放的不算终态（`○` fg3）；完成 `✓` ok，失败 `✗` err（tui-kit.md §3.4、§4.3）。
    assert_eq!(dot(&lines[2]), app.ink.theme.fg3, "parked: no color");
    assert_eq!(dot(&lines[3]).fg_color(), app.ink.theme.ok);
    assert_eq!(dot(&lines[4]).fg_color(), app.ink.theme.err);
    // `(+N)` 是 fg3，连接符是 line 色（Puffin 的轨与分隔的墨）。
    let span = |line: &StyledLine, text: &str| {
        line.spans
            .iter()
            .find(|span| span.text.contains(text))
            .map(|span| span.style)
            .unwrap()
    };
    assert_eq!(span(&lines[2], "(+1)"), app.ink.theme.fg3);
    assert_eq!(span(&lines[2], "├"), app.ink.theme.line);
}

#[test]
fn the_window_follows_the_focus_and_rows_under_a_scrolled_out_parent_start_at_the_left() {
    let mut app = app();
    let mut entries = vec![entry("agent_a", None, "Alpha task")];
    for i in 1..=6 {
        entries.push(entry(
            &format!("agent_c{i}"),
            Some("agent_a"),
            &format!("Child {i}"),
        ));
    }
    set(&mut app, entries);
    view(&mut app, "agent_a");
    // 8 项、焦点在 C6（第 8 项）：kit 的 `picker_window` 给 C1–C6，上面一行 `↑ 2 more`；A 滚出窗口，孩子们顶格，名字列宽仍按
    // 深度 1 算，描述列不动。下面没有更多时没有空行（Puffin 的 picker 不占位）。
    select(&mut app, 7);
    let text = rows(&mut app, 100, T0);
    assert_eq!(text.len(), 1 + 6, "{text:?}");
    assert_eq!(text[0], "     ↑ 2 more");
    assert_eq!(text[1], "   ○ general-purpose    Child 1");
    // 选中只改墨（名字 brand），没有指针（tui-kit.md §4.3）。
    assert_eq!(text[6], "   ○ general-purpose    Child 6");
    // 焦点回到 C1：主会话与 A 在窗口里，孩子画连接符；下面还有 2 项。
    select(&mut app, 2);
    let text = rows(&mut app, 100, T0);
    assert_eq!(text[0], "     main");
    assert_eq!(text[1], "   ○ general-purpose    Alpha task  current");
    assert_eq!(text[2], "   ○ ├ general-purpose  Child 1");
    assert_eq!(text[5], "   ○ ├ general-purpose  Child 4");
    assert_eq!(text[6], "     ↓ 2 more");
}

#[test]
fn the_name_column_fits_the_deepest_row_and_stops_at_28_columns() {
    let mut app = app();
    let mut a = entry("agent_a", None, "Alpha task");
    a["agent_type"] = json!("Explore");
    let mut a1 = entry("agent_a1", Some("agent_a"), "Alpha one");
    a1["agent_type"] = json!("a-really-long-custom-agent-name");
    set(&mut app, vec![a, a1]);
    view(&mut app, "agent_a");
    let text = rows(&mut app, 100, T0);
    // 2 × 1 + 31 > 28：列宽 28，孩子的名字格 26，名字截断。
    assert_eq!(
        text[1],
        format!("   ○ {:<28}  Alpha task  current", "Explore")
    );
    assert_eq!(text[2], "   ○ └ a-really-long-custom-agen…  Alpha one");
    // 描述列对齐（按显示宽度比，圆点与连接符是多字节字符）。
    let column = |row: &str| string_width(&row[..row.find("Alpha").unwrap()]);
    assert_eq!(column(&text[1]), column(&text[2]));
}

#[test]
fn keys_walk_the_tree_in_order() {
    let mut app = app();
    let now = Instant::now();
    set(&mut app, family());
    view(&mut app, "agent_a1");
    // ↓ 打开任务卡，选中正在看的 A1（tui-tasks.md §3.2），再往下按树的顺序：A1a、A2。
    app.on_key(key(KeyCode::Down), now);
    assert_eq!(app.task_card().map(|card| app.agent_row(card)), Some(2));
    app.on_key(key(KeyCode::Down), now);
    assert_eq!(
        app.on_key(ctrl('x'), now),
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

/// 正在查看的、结束了的后台代理照常成行（`current`），结束 30 秒后被驱逐。
#[test]
fn a_viewed_completed_background_agent_is_current() {
    let mut app = app();
    let mut entries = family();
    entries[2] = ended(entries[2].clone(), "completed", 1_000);
    set(&mut app, entries);
    // 看顶层的 B：全部顶层代理，A 不在查看链上，照常写 `(+3)`，名字列按它算。
    view(&mut app, "agent_b");
    assert_eq!(
        rows(&mut app, 100, T0)[1..],
        [
            "   ○ general-purpose (+3)  Alpha task".to_owned(),
            "   ✓ general-purpose       Beta task  current".to_owned(),
        ]
    );
}

#[test]
fn chinese_and_both_themes() {
    let mut chosen = Vec::new();
    for background in [(0, 0, 0), (255, 255, 255)] {
        let mut app = app_on(background);
        app.lang = Lang::Zh;
        let mut entries = vec![entry("agent_a", None, "Alpha task")];
        for i in 1..=6 {
            entries.push(entry(
                &format!("agent_c{i}"),
                Some("agent_a"),
                &format!("Child {i}"),
            ));
        }
        set(&mut app, entries);
        view(&mut app, "agent_a");
        select(&mut app, 7);
        let (lines, _) = layer(&mut app, 100, T0);
        let text = plain(&lines);
        assert_eq!(text[0], "     ↑ 还有 2 个", "{text:?}");
        // 选中的行照 Puffin 的 picker：只有名字换成 `Ink::selection` 的墨（`chosen_style`，`brand`），说明照旧 fg3，用时结束在
        // 量度（宽 − 2）；深浅两种背景各一种（tui-kit.md §3.4、§4.3）。
        let selection = app.ink.selection().fg_color();
        let name = lines[6]
            .spans
            .iter()
            .find(|span| span.text == "general-purpose")
            .unwrap();
        assert_eq!(name.style.fg_color(), selection, "{background:?}");
        let description = lines[6]
            .spans
            .iter()
            .find(|span| span.text == "Child 6")
            .unwrap();
        assert_eq!(description.style, app.ink.theme.fg3, "{background:?}");
        assert_eq!(lines[6].width(), 98, "{background:?}");
        chosen.push(selection);
        select(&mut app, 2);
        let text = plain(&layer(&mut app, 100, T0).0);
        assert_eq!(text[0], "     主会话");
        assert_eq!(text[6], "     ↓ 还有 2 个");
    }
    assert_ne!(chosen[0], chosen[1], "深浅两种背景的选中墨色不同");
}

/// 查看对象不在了（tui-panels.md §1.6）：被查看的子代理不在快照里了 → 改看它的 owner（原始的父代理）；owner 也不在或已驱逐 →
/// 回到主会话；还在快照里时不动。
#[test]
fn a_viewed_agent_that_leaves_the_snapshot_hands_the_view_to_its_owner() {
    let mut app = app();
    set(&mut app, family());
    view(&mut app, "agent_a1a");
    assert_eq!(app.viewed_gone(), None);
    // A1a 不在了：A1 还在跑，改看 A1。
    let mut entries = family();
    entries.remove(4);
    set(&mut app, entries.clone());
    assert_eq!(app.viewed_gone(), Some(Some(AgentId::new("agent_a1"))));
    // A1 已结束 30 秒以上（驱逐）：回到主会话。
    entries[1] = ended(entries[1].clone(), "completed", 31_000);
    set(&mut app, entries);
    assert_eq!(app.viewed_gone(), Some(None));
    // 主线程派生的（没有 owner）不在了：回到主会话。
    set(&mut app, family());
    view(&mut app, "agent_b");
    set(&mut app, vec![entry("agent_a", None, "Alpha task")]);
    assert_eq!(app.viewed_gone(), Some(None));
    // 没在查看时不管。
    app.viewing = None;
    assert_eq!(app.viewed_gone(), None);
}

/// 卡记的是选中的代理，不是第几行（tui-tasks.md §3.2）：查看换了、行挪了，选中的仍是它；它离开了树时选中 main。
#[test]
fn the_card_keeps_its_agent_when_the_rows_move() {
    let mut app = app();
    let now = Instant::now();
    set(&mut app, family());
    // 看 A：main、A、A1、A2、B。选中 A2（第 3 行）。
    view(&mut app, "agent_a");
    app.on_key(key(KeyCode::Down), now);
    app.on_key(key(KeyCode::Down), now);
    app.on_key(key(KeyCode::Down), now);
    let row = |app: &App| app.task_card().map(|card| app.agent_row(card));
    assert_eq!(row(&app), Some(3));
    // 换成看 A1：A1a 插在 A2 前面，A2 挪到第 4 行，选中的跟着它。
    view(&mut app, "agent_a1");
    assert_eq!(row(&app), Some(4));
    assert_eq!(
        app.on_key(key(KeyCode::Enter), now),
        Some(Action::Panel(Panel::ViewAgent(Some(AgentId::new(
            "agent_a2"
        )))))
    );
    // A2 离开了树：选中 main。
    let entries = family()
        .into_iter()
        .filter(|e| e["agent"] != "agent_a2")
        .collect();
    set(&mut app, entries);
    assert_eq!(row(&app), Some(0));
}
