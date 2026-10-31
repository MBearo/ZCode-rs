//! 查看子代理时 footer 跟着切（docs/specs/rust-cli/tui-panels.md §1.7，修复线 F112，用户要求 2026-10-29）：右侧的模型、
//! 档位、TPS、ctx、cache、上下文提示与左侧的权限模式都换成被查看的子代理的；查看时不写 esc 中断与不生效的按键提示；
//! 返回主会话后恢复。
use std::time::Instant;

use rust_cli_kernel_api::compaction::{ContextNotice, ContextOccupancy};
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{AgentId, EnvironmentId, SessionId, TraceId};
use rust_cli_kernel_api::metering::{OutputSpeed, RequestStats};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::panels::{SubagentEntry, SubagentStatus};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{KernelEvent, SequencedEvent, SessionView};
use rust_cli_kernel_api::store::{SessionKind, SessionMeta};
use rust_cli_kernel_api::testing::selection;

use super::*;
use crate::ui::agents::{AgentList, Viewing};
use crate::ui::palette::{ColorLevel, Palette};
use crate::view::Transcript;

fn event(app: &mut App, seq: u64, event: KernelEvent) {
    app.view.apply(
        SequencedEvent {
            seq,
            revision: 0,
            event,
        },
        0,
    );
}

/// 主会话：模型 `scripted`、TPS 40、ctx 37.0%、cache 91.0%、`10% until auto-compact`，回合在跑，输入框为空。
fn main_app(lang: Lang) -> App {
    let mut app = App::new(
        Palette::new(ColorLevel::TrueColor, Some((0, 0, 0))).with_foreground(Some((230, 230, 230))),
        lang,
        Transcript::new(
            selection(),
            PermissionMode::Default,
            crate::view::test_workspace(),
        ),
        Some("/Users/dev".into()),
        "0.1.0".into(),
    );
    app.session = Some("sess_main".into());
    event(
        &mut app,
        1,
        KernelEvent::ContextUsage {
            used_tokens: 66_600,
            max_tokens: 180_000,
        },
    );
    event(
        &mut app,
        2,
        KernelEvent::OutputSpeed {
            speed: OutputSpeed {
                output_tokens: Some(120),
                duration_ms: 5_000,
                first_content_ms: Some(2_000),
            },
        },
    );
    event(
        &mut app,
        3,
        KernelEvent::ContextNotice {
            notice: ContextNotice::UntilAutoCompact { percent: 10 },
        },
    );
    event(
        &mut app,
        4,
        KernelEvent::TurnStarted {
            turn: rust_cli_kernel_api::ids::TurnId::new("turn_1"),
            command: None,
        },
    );
    receipt(&mut app, 2_000, 1_820);
    app
}

/// 一份命中率的回执（累计输入、缓存读）。
fn receipt(app: &mut App, input: u64, read: u64) {
    let seq = app.request_stats.begin();
    assert!(app.request_stats.arrived(
        seq,
        Some(RequestStats {
            requests: 1,
            input_tokens: input,
            cache_read_tokens: read,
            unknown_cache_usage: false,
            last: None,
        })
    ));
}

/// 子代理的快照：自己的模型与档位、权限模式；`stats` 时带 ctx 20.0%（9000 / 45000）与 30 tok/s。
fn subagent_view(mode: PermissionMode, plan: bool, stats: bool) -> Transcript {
    let view = SessionView {
        meta: SessionMeta {
            id: SessionId::new("sess_subagent_agent_a"),
            kind: SessionKind::Subagent,
            parent: Some(SessionId::new("sess_main")),
            workspace: WorkspaceRef {
                environment: EnvironmentId::new("local"),
                root: EnvPath::new("/Users/dev/project"),
            },
            title: "Find callers".into(),
            trace: TraceId::new("sess_main"),
            created_at_ms: 0,
        },
        messages: Vec::new(),
        selection: ModelSelection {
            model: "child-model".into(),
            reasoning: Some("high".into()),
            ..selection()
        },
        mode,
        plan,
        pending: Vec::new(),
        queue: Vec::new(),
        last_seq: 0,
        revision: 0,
        running: true,
        background: Vec::new(),
        peer_held: Vec::new(),
        context_usage: stats.then_some(ContextOccupancy {
            used_tokens: 9_000,
            max_tokens: 45_000,
        }),
        context_notice: ContextNotice::Hidden,
        output_speed: stats.then_some(OutputSpeed {
            output_tokens: Some(90),
            duration_ms: 4_000,
            first_content_ms: Some(1_000),
        }),
    };
    Transcript::from_view(view, 0)
}

fn entry() -> SubagentEntry {
    SubagentEntry {
        agent: AgentId::new("agent_a"),
        child: SessionId::new("sess_subagent_agent_a"),
        agent_type: "Explore".into(),
        description: "Find callers".into(),
        status: SubagentStatus::Running,
        background: true,
        waiting: false,
        elapsed_ms: 5_000,
        ended_ago_ms: None,
        version: 0,
        owner: None,
        activity: None,
        parked: false,
        machine: None,
    }
}

/// 开始查看 agent_a：runner 在查看对象换了时清空命中率的回执再查子会话（这里直接给回执）。
fn view(app: &mut App, view: Transcript) {
    app.agents = AgentList {
        entries: vec![entry()],
        at: Some(Instant::now()),
    };
    app.viewing = Some(Viewing {
        agent: AgentId::new("agent_a"),
        kind: "Explore".into(),
        description: "Find callers".into(),
        version: 0,
        status: SubagentStatus::Running,
        messages: Vec::new(),
        owner: None,
        view,
    });
    app.request_stats.clear();
}

fn plain(app: &App, width: usize) -> String {
    crate::ui::text::plain(&footer(app, width).0)
}

/// 同一个 App 不在查看时的 footer（对照用：拿走查看再放回）。
fn main_tip_without_viewing(app: &mut App) -> String {
    let viewing = app.viewing.take();
    let text = plain(app, 160);
    app.viewing = viewing;
    text
}

/// 右侧换成子代理的模型、档位、TPS、ctx 与 cache；主会话的值（含上下文提示）一个都不留。修前 footer 只读主会话。
#[test]
fn viewing_shows_the_subagents_model_effort_and_stats() {
    let mut app = main_app(Lang::En);
    let before = plain(&app, 160);
    assert!(
        before.ends_with(
            "40 tok/s \u{b7} ctx 37.0% \u{b7} cache 91.0% \u{b7} 10% until auto-compact \u{b7} scripted"
        ),
        "{before}"
    );
    view(
        &mut app,
        subagent_view(PermissionMode::Default, false, true),
    );
    receipt(&mut app, 400, 200);
    let text = plain(&app, 160);
    assert!(
        text.ends_with("30 tok/s \u{b7} ctx 20.0% \u{b7} cache 50.0% \u{b7} child-model high"),
        "{text}"
    );
    for main in [
        "scripted",
        "40 tok/s",
        "ctx 37.0%",
        "cache 91.0%",
        "auto-compact",
    ] {
        assert!(!text.contains(main), "{main}: {text}");
    }
    // 模型名照旧用 `model` 的颜色，统计用次要文字。
    let (line, _) = footer(&app, 160);
    let model = line
        .spans
        .iter()
        .find(|s| s.content == "child-model high")
        .unwrap();
    assert_eq!(model.style, app.palette.model());
    let ctx = line
        .spans
        .iter()
        .find(|s| s.content == "ctx 20.0%")
        .unwrap();
    assert_eq!(ctx.style, app.palette.secondary());
}

/// 子代理没有的项不出现，也不拿主会话的值顶替：还没有完成的请求（没有 TPS、没有 ctx）、命中率的回执还没到。
#[test]
fn missing_subagent_stats_are_left_out() {
    let mut app = main_app(Lang::En);
    view(
        &mut app,
        subagent_view(PermissionMode::Default, false, false),
    );
    let text = plain(&app, 160);
    assert!(text.ends_with("child-model high"), "{text}");
    for main in ["tok/s", "ctx", "cache", "auto-compact"] {
        assert!(!text.contains(main), "{main}: {text}");
    }
}

/// 子代理的上下文提示与 ctx 同一处算，跟着切（暂定，tui-panels.md D20）。
#[test]
fn the_context_notice_is_the_subagents() {
    let mut app = main_app(Lang::En);
    let mut sub = SessionView {
        context_notice: ContextNotice::Low {
            percent: 8,
            compact_hint: false,
        },
        ..snapshot_of(subagent_view(PermissionMode::Default, false, true))
    };
    sub.running = false;
    view(&mut app, Transcript::from_view(sub, 0));
    let text = plain(&app, 160);
    assert!(
        text.ends_with("Context low (8% remaining) \u{b7} child-model high"),
        "{text}"
    );
    assert!(!text.contains("until auto-compact"), "{text}");
}

/// 左侧：权限模式与 plan 写子代理的；不写 `esc to interrupt`（esc 是返回），也不写查看时不生效的按键提示；上滚时照旧
/// 写 `esc latest`。修前主会话在跑时写 `esc to interrupt`，模式是主会话的。
#[test]
fn the_left_side_follows_the_subagent_without_main_hints() {
    let mut app = main_app(Lang::En);
    // 主会话有一个在跑的 shell（pill）。
    app.background = vec![rust_cli_kernel_api::manage::BackgroundEntry {
        id: "bash_1".into(),
        kind: rust_cli_kernel_api::manage::BackgroundKind::Shell,
        description: "npm run dev".into(),
        status: rust_cli_kernel_api::env_shell::BackgroundStatus::Running,
        output: None,
        machine: None,
        reconnecting: false,
    }];
    let before = plain(&app, 160);
    assert!(before.contains("Ask before changes"), "{before}");
    assert!(before.contains("esc to interrupt"), "{before}");
    view(
        &mut app,
        subagent_view(PermissionMode::AcceptEdits, true, true),
    );
    let text = plain(&app, 160);
    assert!(
        text.starts_with(
            "  \u{23f8} Plan mode \u{b7} \u{23f5}\u{23f5} Edit automatically \u{b7} 1 shell"
        ),
        "{text}"
    );
    for hint in [
        "Ask before changes",
        "esc to interrupt",
        "to manage",
        "for projects",
        "for shortcuts",
    ] {
        assert!(!text.contains(hint), "{hint}: {text}");
    }
    // 主会话空闲、有任务清单时的 `ctrl+t` 提示也不写。
    event(
        &mut app,
        5,
        KernelEvent::TurnFinished {
            turn: rust_cli_kernel_api::ids::TurnId::new("turn_1"),
            outcome: rust_cli_kernel_api::session_api::TurnOutcome::Success,
            usage: None,
        },
    );
    event(
        &mut app,
        6,
        KernelEvent::TasksChanged {
            tasks: serde_json::json!([{"id": "1", "subject": "task 1", "description": "",
                                       "status": "pending", "blocks": [], "blockedBy": []}]),
        },
    );
    app.background.clear();
    assert!(
        main_tip_without_viewing(&mut app).contains("ctrl+t"),
        "主会话空闲、有任务清单时写 ctrl+t 的提示"
    );
    let text = plain(&app, 160);
    assert!(!text.contains("ctrl+t"), "{text}");
    assert!(!text.contains("for projects"), "{text}");
    // 上滚时 esc 先回到底部：照旧提示。
    app.scroll = 3;
    let text = plain(&app, 160);
    assert!(text.contains("esc latest"), "{text}");
}

/// 返回主会话：模型、TPS、ctx、提示立即是主会话的（它们一直在主会话的视图里），cache 等新回执；左侧恢复。
#[test]
fn returning_to_main_restores_it() {
    let mut app = main_app(Lang::En);
    view(
        &mut app,
        subagent_view(PermissionMode::AcceptEdits, false, true),
    );
    receipt(&mut app, 400, 200);
    app.viewing = None;
    app.request_stats.clear();
    let text = plain(&app, 160);
    assert!(
        text.ends_with("40 tok/s \u{b7} ctx 37.0% \u{b7} 10% until auto-compact \u{b7} scripted"),
        "{text}"
    );
    assert!(text.contains("Ask before changes"), "{text}");
    assert!(text.contains("esc to interrupt"), "{text}");
    receipt(&mut app, 2_000, 1_820);
    assert!(plain(&app, 160).contains("\u{b7} cache 91.0% \u{b7}"));
}

/// 换查看对象之前发出的命中率查询，回执晚到时丢掉（序号），不显示在新对象下面。
#[test]
fn a_receipt_from_before_the_switch_is_dropped() {
    let mut app = main_app(Lang::En);
    let in_flight = app.request_stats.begin();
    view(
        &mut app,
        subagent_view(PermissionMode::Default, false, true),
    );
    assert!(!app.request_stats.arrived(
        in_flight,
        Some(RequestStats {
            requests: 1,
            input_tokens: 2_000,
            cache_read_tokens: 1_820,
            unknown_cache_usage: false,
            last: None,
        })
    ));
    assert!(!plain(&app, 160).contains("cache"));
}

/// 查谁的命中率：没有会话时不查；看主会话时是主会话；查看时是列表快照里那个条目的子会话；条目不在了不查。
#[test]
fn the_stats_follow_the_viewed_session() {
    let mut app = main_app(Lang::En);
    assert_eq!(app.stats_session(), Some(SessionId::new("sess_main")));
    view(
        &mut app,
        subagent_view(PermissionMode::Default, false, true),
    );
    assert_eq!(
        app.stats_session(),
        Some(SessionId::new("sess_subagent_agent_a"))
    );
    app.agents.entries.clear();
    assert_eq!(app.stats_session(), None);
    app.viewing = None;
    app.session = None;
    assert_eq!(app.stats_session(), None);
}

/// 中文：右侧的短词与左侧的模式名按界面语言。
#[test]
fn chinese_uses_the_chinese_words() {
    let mut app = main_app(Lang::Zh);
    view(
        &mut app,
        subagent_view(PermissionMode::AcceptEdits, false, true),
    );
    receipt(&mut app, 400, 200);
    let text = plain(&app, 160);
    assert!(
        text.ends_with("30 tok/s \u{b7} 上下文 20.0% \u{b7} 缓存 50.0% \u{b7} child-model high"),
        "{text}"
    );
    assert!(text.contains("自动编辑"), "{text}");
    assert!(!text.contains("esc 中断"), "{text}");
}

/// 窄终端：查看时省略顺序不变——cache、TPS、ctx、上下文提示，最后才省模型名。
#[test]
fn narrow_terminals_keep_the_order_while_viewing() {
    let mut app = main_app(Lang::En);
    let mut sub = snapshot_of(subagent_view(PermissionMode::Default, false, true));
    sub.context_notice = ContextNotice::UntilAutoCompact { percent: 7 };
    view(&mut app, Transcript::from_view(sub, 0));
    receipt(&mut app, 400, 200);
    let mut seen: Vec<String> = Vec::new();
    for width in (20..=160).rev() {
        let text = plain(&app, width);
        // 按显示的先后列：统计、提示，模型名贴最右（F116）。
        let right: Vec<&str> = [
            "30 tok/s",
            "ctx 20.0%",
            "cache 50.0%",
            "7% until auto-compact",
            "child-model high",
        ]
        .into_iter()
        .filter(|item| text.contains(item))
        .collect();
        let right = right.join(" | ");
        if seen.last() != Some(&right) {
            seen.push(right);
        }
    }
    assert_eq!(
        seen,
        [
            "30 tok/s | ctx 20.0% | cache 50.0% | 7% until auto-compact | child-model high",
            "30 tok/s | ctx 20.0% | 7% until auto-compact | child-model high",
            "ctx 20.0% | 7% until auto-compact | child-model high",
            "7% until auto-compact | child-model high",
            "child-model high",
            "",
        ]
    );
}

/// 测试里改快照的一项：把子代理视图的设置还原成快照（只取 footer 用到的字段）。
fn snapshot_of(view: Transcript) -> SessionView {
    SessionView {
        meta: SessionMeta {
            id: SessionId::new("sess_subagent_agent_a"),
            kind: SessionKind::Subagent,
            parent: Some(SessionId::new("sess_main")),
            workspace: view.workspace().clone(),
            title: view.title().to_owned(),
            trace: TraceId::new("sess_main"),
            created_at_ms: 0,
        },
        messages: Vec::new(),
        selection: view.selection().clone(),
        mode: view.mode(),
        plan: view.plan(),
        pending: Vec::new(),
        queue: Vec::new(),
        last_seq: 0,
        revision: 0,
        running: view.running(),
        background: Vec::new(),
        peer_held: Vec::new(),
        context_usage: view.context_usage(),
        context_notice: view.context_notice(),
        output_speed: view.output_speed(),
    }
}
