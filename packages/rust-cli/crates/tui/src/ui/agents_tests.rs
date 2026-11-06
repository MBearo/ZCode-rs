//! 子代理列表（tui-panels.md §1；任务卡的 Subagents，tui-tasks.md §3.2）：可见规则、排版与按键。
use std::time::Instant;

use crossterm::event::KeyCode;
use rust_cli_kernel_api::ids::{AgentId, SessionId};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::selection;

use super::*;
use crate::ui::i18n::Lang;
use crate::ui::manage::Manage;
use crate::ui::test_support::{app, ctrl, key};

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
        machine: Default::default(),
    }
}

/// 任务卡的 Subagents（选中 main）：排好的行与（行，选择下标）。
/// `shown` 是这一帧的墙上时间（`draw::draw` 写进 `AgentList::shown`）。
fn layer(app: &mut App, width: usize, shown: u64) -> (Vec<StyledLine>, Vec<(usize, usize)>) {
    app.snapshots.agents.shown = shown;
    app.subagent_rows(0, width)
}

/// 快照到达的墙上时间（Unix 毫秒）。
const T0: u64 = 1_800_000_000_000;

fn plain(lines: &[StyledLine]) -> Vec<String> {
    lines.iter().map(StyledLine::plain).collect()
}

#[test]
fn ended_agents_leave_the_list_thirty_seconds_after_they_end() {
    let mut app = app();
    app.snapshots.agents = AgentList {
        entries: vec![
            entry("agent_a", SubagentStatus::Running, None),
            entry("agent_b", SubagentStatus::Completed, Some(29_000)),
            entry("agent_c", SubagentStatus::Failed, Some(31_000)),
        ],
        at: Some(T0),
        shown: T0,
    };
    let ids = |app: &mut App, shown| {
        app.snapshots.agents.shown = shown;
        app.agent_rows()
            .iter()
            .map(|e| e.agent.to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&mut app, T0), ["agent_a", "agent_b"]);
    // 快照到达后又过了 2 秒（墙上时间）：agent_b 也到了 30 秒。
    assert_eq!(ids(&mut app, T0 + 2_000), ["agent_a"]);
    // 在跑的行的用时往后推，结束的不变。
    app.snapshots.agents.shown = T0 + 5_000;
    assert_eq!(
        elapsed_ms(&app.snapshots.agents, &app.snapshots.agents.entries[0]),
        70_000
    );
    assert_eq!(
        elapsed_ms(&app.snapshots.agents, &app.snapshots.agents.entries[1]),
        65_000
    );
    // 快照之后、下一帧之前（画面的时刻还在快照之前）：不往回推。
    app.snapshots.agents.shown = T0 - 1_000;
    assert_eq!(
        elapsed_ms(&app.snapshots.agents, &app.snapshots.agents.entries[0]),
        65_000
    );
}

#[test]
fn the_list_has_main_then_one_line_per_agent() {
    let mut app = app();
    let mut waiting = entry("agent_a", SubagentStatus::Running, None);
    waiting.waiting = true;
    waiting.background = false;
    // 前台完成的照常留 30 秒；后台完成的立即隐藏（修复线 F30，tui-panels.md §1.2，见 agents_tree_tests.rs）。
    let mut done = entry("agent_b", SubagentStatus::Completed, Some(0));
    done.background = false;
    app.snapshots.agents = AgentList {
        entries: vec![waiting, done],
        at: Some(T0),
        shown: T0,
    };
    let (lines, targets) = layer(&mut app, 80, T0);
    let text = plain(&lines);
    // 板上的一层，照 Puffin `/resume` 的列表（tui-kit.md §4.3）：没有头，主会话在前，正在看的写 fg3 的 `current`。
    assert_eq!(text[0], "     main     current");
    assert!(
        text[1].starts_with("   ● Explore  Look at agent_a  waiting for you "),
        "{text:?}"
    );
    // 用时列：在跑的是走着的整秒，结束的是用了多久（Puffin 的写法）；还不知道的 token 与速度两列空着、照样占着宽度，
    // 右沿是量度（宽 − 2）。
    assert!(text[1].trim_end().ends_with(" 1m 05s"), "{text:?}");
    assert!(
        text[2].starts_with("   ✓ Explore  Look at agent_b "),
        "{text:?}"
    );
    assert!(text[2].trim_end().ends_with(" 1m 05s"), "{text:?}");
    assert_eq!(string_width(&text[2]), 78);
    assert_eq!(targets, [(0, 0), (1, 1), (2, 2)]);
    // 记号的墨：等你回答 code，完成 ok。
    let mark = |line: &StyledLine, text: &str| {
        line.spans
            .iter()
            .find(|span| span.text == text)
            .map(|span| span.style.fg_color())
            .unwrap()
    };
    assert_eq!(mark(&lines[1], "●"), app.ink.theme.code);
    assert_eq!(mark(&lines[1], "  waiting for you"), app.ink.theme.code);
    assert_eq!(mark(&lines[2], "✓"), app.ink.theme.ok);
}

#[test]
fn more_than_five_agents_end_with_a_count() {
    let mut app = app();
    app.snapshots.agents = AgentList {
        entries: (0..7)
            .map(|i| entry(&format!("agent_{i}"), SubagentStatus::Running, None))
            .collect(),
        at: Some(T0),
        shown: T0,
    };
    // 8 项（主会话 + 7）超过 Puffin picker 的 7 行：主会话与 5 个，然后一行 `↓ 2 more`（名字的列，fg3）。
    let text = plain(&layer(&mut app, 80, T0).0);
    assert_eq!(text.len(), 1 + 5 + 1);
    assert_eq!(text.last().unwrap(), "     ↓ 2 more");
}

/// 任务卡的 Subagents（tui-tasks.md §3.3）：↓ 打开（选中 main），↑↓ 移动，`^X` 停止在跑的后台子代理，⏎ 查看并关上卡。
#[test]
fn keys_move_view_stop_and_leave() {
    let mut app = app();
    let now = Instant::now();
    let mut foreground = entry("agent_f", SubagentStatus::Running, None);
    foreground.background = false;
    app.snapshots.agents = AgentList {
        entries: vec![entry("agent_b", SubagentStatus::Running, None), foreground],
        at: Some(T0),
        shown: T0,
    };
    let selected = |app: &App| app.task_card().map(|card| app.agent_row(card));
    // 输入框为空时 ↓ 打开任务卡，选中第一个子代理（tui-tasks.md §3.2）。
    assert_eq!(app.on_key(key(KeyCode::Down), now), None);
    assert_eq!(selected(&app), Some(1));
    // ^X 停止后台子代理：与 `/tasks` 同一条路径。
    let ctrl_x = ctrl('x');
    assert_eq!(
        app.on_key(ctrl_x, now),
        Some(Action::Manage(Manage::StopTask {
            id: "agent_b".into(),
            label: "Look at agent_b".into(),
        }))
    );
    // 前台子代理不能停。
    app.on_key(key(KeyCode::Down), now);
    assert_eq!(app.on_key(ctrl_x, now), None);
    assert_eq!(
        app.on_key(key(KeyCode::Enter), now),
        Some(Action::Panel(Panel::ViewAgent(Some(AgentId::new(
            "agent_f"
        )))))
    );
    // 卡留着，选中的仍是它（tui-tasks.md §3.3）。
    assert_eq!(selected(&app), Some(2));
    // 在 main 上按 ↑ 停住；esc 关上卡。
    for _ in 0..3 {
        app.on_key(key(KeyCode::Up), now);
    }
    assert_eq!(selected(&app), Some(0));
    app.on_key(key(KeyCode::Esc), now);
    assert_eq!(selected(&app), None);
}

#[test]
fn viewing_is_read_only_and_esc_returns() {
    let mut app = app();
    let now = Instant::now();
    app.snapshots.agents = AgentList {
        entries: vec![entry("agent_a", SubagentStatus::Running, None)],
        at: Some(T0),
        shown: T0,
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
    assert_eq!(header[1], " ↳ Explore · Look at agent_a");
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
    let with = |id: &str, activity: SubagentActivity| {
        let mut row = entry(id, SubagentStatus::Running, None);
        row.activity = Some(activity);
        row
    };
    app.snapshots.agents = AgentList {
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
        at: Some(T0),
        shown: T0,
    };
    let later = T0 + 3_000;
    let (lines, _) = layer(&mut app, 100, later);
    let text = plain(&lines);
    assert!(text[1].contains("Look at agent_a "), "{text:?}");
    assert!(!text[1].contains("Thinking"), "{text:?}");
    assert!(text[2].contains("Look at agent_b "), "{text:?}");
    assert!(!text[2].contains("Bash"), "{text:?}");
    assert!(
        text[3].contains("Look at agent_c  Retrying in 6s · attempt 2/10 "),
        "{text:?}"
    );
    assert!(
        text[4].contains("Look at agent_d  Waiting for API response "),
        "{text:?}"
    );
    for row in &text[1..5] {
        assert!(row.trim_end().ends_with(" 1m 08s"), "{text:?}");
    }
    let doing = |line: &StyledLine, what: &str| {
        line.spans
            .iter()
            .find(|span| span.text.contains(what))
            .map(|span| span.style.fg_color())
    };
    assert_eq!(doing(&lines[3], "Retrying"), Some(app.ink.theme.err));
    assert_eq!(doing(&lines[4], "Waiting for API"), Some(app.ink.theme.err));
    // 中文。
    app.lang = Lang::Zh;
    let text = plain(&layer(&mut app, 100, later).0);
    assert!(!text[1].contains("思考中"), "{text:?}");
    assert!(text[3].contains("  6s后重试 · 第 2/10 次 "), "{text:?}");
    assert!(text[4].contains("  等待 API 响应 "), "{text:?}");
    // 等你回答时只写它（Puffin 名单的 `waiting for you`）。
    app.lang = Lang::En;
    app.snapshots.agents.entries[0].waiting = true;
    let text = plain(&layer(&mut app, 100, later).0);
    assert!(
        text[1].contains("Look at agent_a  waiting for you "),
        "{text:?}"
    );
    assert!(text[1].trim_end().ends_with(" 1m 08s"), "{text:?}");
}

/// 缺陷修复（修复线 F26）：活动变化时右侧的用时不左右跳。原来用时排在活动前面、描述按两者合计截断，活动从
/// `∴ Thinking` 换成 `Bash` 时用时往右移了 6 列（用户截图里的 `13s` / `12s`）。
#[test]
fn the_duration_stays_in_place_when_the_activity_changes() {
    let mut app = app();
    let mut row = entry("agent_a", SubagentStatus::Running, None);
    row.elapsed_ms = 13_000;
    let column = |app: &mut App| {
        let text = plain(&layer(app, 80, T0).0);
        let line = text[1].clone();
        let at = line.find("13s").expect("the duration");
        (string_width(&line[..at]), string_width(&line))
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
        app.snapshots.agents = AgentList {
            entries: vec![row.clone()],
            at: Some(T0),
            shown: T0,
        };
        seen.push(column(&mut app));
    }
    assert!(seen.iter().all(|c| *c == seen[0]), "{seen:?}");
    // 用时结束在量度的右沿（宽 − 2，Puffin 子代理的数字列的右沿）。
    assert_eq!(seen[0].1, 78, "the row ends at the measure");
    // 很窄时活动与描述让位，用时仍在行尾。
    row.activity = Some(SubagentActivity::Tool {
        name: "a_tool_with_a_very_long_name_indeed".into(),
    });
    app.snapshots.agents = AgentList {
        entries: vec![row],
        at: Some(T0),
        shown: T0,
    };
    let text = plain(&layer(&mut app, 40, T0).0);
    assert!(text[1].ends_with(" 13s"), "{text:?}");
    assert!(string_width(&text[1]) <= 40, "{text:?}");
}

/// 远端执行 R4（remote-exec.md §21.6）：子代理的缺省机器在远端时，类型后面标 ` @机器`（强调色），描述列照旧对齐。
#[test]
fn agents_on_a_remote_machine_name_it() {
    let mut app = app();
    let mut remote = entry("agent_a", SubagentStatus::Running, None);
    remote.machine = rust_cli_kernel_api::ids::MachineName::new("dev-a");
    let local = entry("agent_b", SubagentStatus::Running, None);
    app.snapshots.agents = AgentList {
        entries: vec![remote, local],
        at: Some(T0),
        shown: T0,
    };
    let (lines, _) = layer(&mut app, 80, T0);
    let text = plain(&lines);
    assert!(
        text[1].starts_with("   ○ Explore @dev-a  Look at agent_a"),
        "{text:?}"
    );
    assert!(
        text[2].starts_with("   ○ Explore         Look at agent_b"),
        "{text:?}"
    );
    let tag = lines[1]
        .spans
        .iter()
        .find(|span| span.text == " @dev-a")
        .unwrap();
    assert_eq!(tag.style.fg_color(), app.ink.accent());
}

/// 钉住子代理的层的画面（tui-kit.md §4.3「子代理列表与只读查看」，Puffin 没有这一面）：照 Puffin `/resume` 的列表——没有头、
/// 没有指针，第 3 列是 Puffin 的记号，名字 fg2（选中的 brand），说明 fg3，状态接在说明后面，正在看的写 fg3 的 `current`；
/// 行尾是 Puffin 子代理的三列数字（fg3，止于量度 = 宽 − 2；这里还没有查来的数字，token 与速度两列空着）。
#[test]
fn the_layer_is_drawn_like_puffins_picker() {
    let mut app = app();
    let mut waiting = entry("agent_a", SubagentStatus::Running, None);
    waiting.waiting = true;
    waiting.background = false;
    let running = entry("agent_b", SubagentStatus::Running, None);
    let failed = entry("agent_c", SubagentStatus::Failed, Some(1_000));
    app.snapshots.agents = AgentList {
        entries: vec![waiting, running, failed],
        at: Some(T0),
        shown: T0,
    };
    let (lines, targets) = app.subagent_rows(2, 80);
    let row = |left: &str| format!("{left:<50}1m 05s{}", " ".repeat(22));
    assert_eq!(
        plain(&lines),
        [
            "     main     current".to_owned(),
            row("   ● Explore  Look at agent_a  waiting for you"),
            row("   ○ Explore  Look at agent_b"),
            row("   ✗ Explore  Look at agent_c"),
        ]
    );
    assert_eq!(targets, [(0, 0), (1, 1), (2, 2), (3, 3)]);
    let theme = app.ink.theme;
    let ink = |line: usize, text: &str| {
        lines[line]
            .spans
            .iter()
            .find(|span| span.text == text)
            .map(|span| span.style)
            .unwrap_or_else(|| panic!("{text:?} on row {line}"))
    };
    assert_eq!(ink(0, "main"), theme.fg2);
    assert_eq!(ink(0, "  current"), theme.fg3);
    assert_eq!(ink(1, "●").fg_color(), theme.code);
    assert_eq!(ink(1, "Explore"), theme.fg2);
    assert_eq!(ink(1, "Look at agent_a"), theme.fg3);
    assert_eq!(ink(1, "  waiting for you").fg_color(), theme.code);
    assert_eq!(ink(1, "1m 05s"), theme.fg3);
    assert_eq!(ink(2, "○"), theme.fg3);
    assert_eq!(ink(2, "Explore"), app.ink.selection());
    assert_eq!(ink(3, "✗").fg_color(), theme.err);
    // 中文：主会话、`current` 与等你回答照 i18n。
    app.lang = Lang::Zh;
    let text = plain(&app.subagent_rows(2, 80).0);
    assert_eq!(text[0], "     主会话   当前");
    assert!(
        text[1].starts_with("   ● Explore  Look at agent_a  等你回答 "),
        "{text:?}"
    );
}

/// 三列数字（tui-panels.md §1.3，2026-11-02 补齐；以前只有用时一列）：数字按子会话查来，在跑的是子会话的 token 和与
/// 最近一次的速度；结束了的取派生它的 Agent 调用报告的 token（同 transcript 的 Agent 行）与平均速度；子代理派生的子代理
/// 主会话里没有 Agent 调用，token 是子会话的和。窄时先去掉速度，再去掉 token。
#[test]
fn rows_carry_the_subagent_figures() {
    use rust_cli_kernel_api::ids::ToolUseId;
    use rust_cli_kernel_api::message::ToolResultContent;
    use rust_cli_kernel_api::session_api::{KernelEvent, SequencedEvent};
    use rust_cli_kernel_api::tool::ToolOutput;

    use crate::view::SubagentFigures;

    let mut app = app();
    let mut feed = |seq: u64, event: KernelEvent| {
        app.view.apply(
            SequencedEvent {
                seq,
                revision: 0,
                event,
            },
            seq * 1000,
        );
    };
    // agent_a 是主会话里 Agent 调用 `a1` 派生的、已经结束、报告了 309k；agent_b 在跑；agent_c 是 agent_a 派生的、已经结束。
    feed(
        1,
        KernelEvent::ToolStarted {
            tool_use: ToolUseId::new("a1"),
            name: "Agent".into(),
            input: serde_json::json!({"description": "look", "prompt": "p"}),
        },
    );
    feed(
        3,
        KernelEvent::ToolDone {
            tool_use: ToolUseId::new("a1"),
            output: ToolOutput {
                content: vec![ToolResultContent::Text {
                    text: "Done.\n<usage>subagent_tokens: 309000\ntool_uses: 9\nduration_ms: 42000</usage>"
                        .into(),
                }],
                is_error: false,
                // Agent 工具的结果带子会话（kernel-api `agents::DISPLAY_KEY`）：Agent 调用由它关联到子会话。
                display: serde_json::json!({"agent": {
                    "agentId": "agent_a",
                    "childSessionId": "sess_subagent_agent_a",
                    "agentType": "Explore",
                }}),
            },
        },
    );
    let figures = |tokens: u64, last: f64, avg: f64| SubagentFigures {
        tokens: Some(tokens),
        last: Some(last),
        avg: Some(avg),
    };
    for (id, f) in [
        ("agent_a", figures(512_000, 90.0, 71.4)),
        ("agent_b", figures(29_000, 70.0, 55.0)),
        ("agent_c", figures(1_200, 40.0, 33.0)),
    ] {
        app.view
            .set_figures(SessionId::new(format!("sess_subagent_{id}")), f);
    }
    let mut done = entry("agent_a", SubagentStatus::Completed, Some(0));
    done.background = false;
    let running = entry("agent_b", SubagentStatus::Running, None);
    let mut nested = entry("agent_c", SubagentStatus::Completed, Some(0));
    nested.background = false;
    nested.owner = Some(AgentId::new("agent_a"));
    app.snapshots.agents = AgentList {
        entries: vec![done, running, nested],
        at: Some(T0),
        shown: T0,
    };
    // 看着 agent_a，它派生的 agent_c 才在它下面成行（tui-panels.md §1.2）。
    app.viewing = Some(Viewing {
        agent: AgentId::new("agent_a"),
        kind: "Explore".into(),
        description: String::new(),
        version: 0,
        status: SubagentStatus::Completed,
        view: Transcript::new(
            selection(),
            PermissionMode::Default,
            crate::view::test_workspace(),
        ),
        messages: Vec::new(),
        owner: None,
    });
    let text = plain(&layer(&mut app, 100, T0).0);
    let line = |needle: &str| {
        text.iter()
            .find(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("{needle} in {text:?}"))
            .clone()
    };
    // 结束的：报告的 token（不是子会话的和 512k）与平均速度。
    assert!(
        line("agent_a").ends_with("1m 05s   309k tok   71 tok/s"),
        "{text:?}"
    );
    // 在跑的：子会话的和与最近一次的速度。
    assert!(
        line("agent_b").ends_with("1m 05s    29k tok   70 tok/s"),
        "{text:?}"
    );
    // 子代理派生的：主会话里没有它的 Agent 调用，token 是子会话的和。
    assert!(
        line("agent_c").ends_with("1m 05s   1.2k tok   33 tok/s"),
        "{text:?}"
    );
    // 行的右沿仍是量度（宽 − 2）。
    assert_eq!(string_width(&line("agent_b")), 98);
    // 数 fg3，单位 quiet。
    let (lines, _) = layer(&mut app, 100, T0);
    let theme = app.ink.theme;
    let at = text.iter().position(|l| l.contains("agent_b")).unwrap();
    let ink = |t: &str| {
        lines[at]
            .spans
            .iter()
            .find(|span| span.text == t)
            .map(|span| span.style)
            .unwrap_or_else(|| panic!("{t:?}"))
    };
    assert_eq!(ink("29k"), theme.fg3);
    assert_eq!(ink(" tok"), theme.quiet);
    assert_eq!(ink(" tok/s"), theme.quiet);
    // 窄的时候（行自己的字至少留 24 格，kit `keep_for`）：50 列先去掉速度，40 列再去掉 token，用时留着。
    let narrow = |app: &mut App, width: usize| {
        // 描述截短了，按位置取 agent_b 的行（main、agent_a、它下面的 agent_c、agent_b）。
        plain(&layer(app, width, T0).0).swap_remove(3)
    };
    let at50 = narrow(&mut app, 50);
    assert!(at50.ends_with("1m 05s    29k tok"), "{at50:?}");
    let at40 = narrow(&mut app, 40);
    assert!(at40.ends_with(" 1m 05s"), "{at40:?}");
}
