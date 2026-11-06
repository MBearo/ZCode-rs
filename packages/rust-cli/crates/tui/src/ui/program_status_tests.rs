//! 程序状态（OSC 7501，tui-manage.md §5.4）：探测回复的判定、编码、根记录的条件与顺序、只在变化时写、子记录与 `clear`。
use rust_cli_kernel_api::ids::{InteractionId, ToolUseId, TurnId};
use rust_cli_kernel_api::manage::BackgroundEntry;
use rust_cli_kernel_api::session_api::{KernelEvent, SequencedEvent, TurnOutcome};
use serde_json::json;

use super::*;
use crate::ui::i18n::Lang;

fn app() -> App {
    crate::ui::test_support::app_in(Lang::En)
}

/// 事件的序号：全局递增，每个视图里也就递增。
static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// 视图收到一个内核事件（序号比上一个大）。
fn apply(app: &mut App, event: KernelEvent) {
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    app.view.apply(
        SequencedEvent {
            seq,
            revision: 0,
            event,
        },
        0,
    );
}

fn started(app: &mut App) {
    apply(
        app,
        KernelEvent::TurnStarted {
            turn: TurnId::new("turn_1"),
            command: None,
        },
    );
}

/// 回合结束；`seen` 时像 runner 一样记下亲眼看到了 `TurnFinished`。
fn finished(app: &mut App, outcome: TurnOutcome, seen: bool) {
    apply(
        app,
        KernelEvent::TurnFinished {
            turn: TurnId::new("turn_1"),
            outcome,
            usage: None,
        },
    );
    if seen {
        app.program_status.turn_finished();
    }
}

fn bash() -> InteractionRequest {
    InteractionRequest::Permission {
        machine: Default::default(),
        tool_use: ToolUseId::new("tu1"),
        tool: "Bash".into(),
        input: json!({"command": "touch a.txt"}),
        message: "This command requires approval".into(),
        suggestions: Vec::new(),
        rule_seed: None,
        asked_by: None,
    }
}

fn ask(app: &mut App, request: InteractionRequest) {
    apply(
        app,
        KernelEvent::InteractionAsked {
            interaction: InteractionId::new("i1"),
            request,
        },
    );
}

/// 标准 base64 的文字。
fn b64(text: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(text)
}

fn root_body(app: &App) -> String {
    root(app).body()
}

#[test]
fn only_a_7501_answer_counts_as_support() {
    assert!(supported(
        "\x1b]7501;?:states=idle,working,done,blocked,error\x1b\\\x1b[?62c"
    ));
    assert!(supported("\x1b]7501;?\x07"));
    assert!(!supported("\x1b]11;rgb:0000/0000/0000\x1b\\\x1b[?62c"));
    assert!(!supported(""));
    assert_eq!(QUERY, "\x1b]7501;?\x1b\\");
}

/// 键的顺序固定；根记录带 `app`，子记录带 `id`、不带 `app`；文字是带补齐的标准 base64，空的不写。
#[test]
fn reports_encode_keys_in_order() {
    let report = Report {
        id: None,
        state: State::Blocked(Kind::Question),
        title: String::new(),
        msg: "ZCode needs your input".into(),
    };
    assert_eq!(
        sequence(&report.body()),
        format!(
            "\x1b]7501;state=blocked:kind=question:app=zcode:msg={}\x1b\\",
            b64("ZCode needs your input")
        )
    );
    let child = Report {
        id: Some("agent-00ff".into()),
        state: State::Working,
        title: "Find the bug".into(),
        msg: String::new(),
    };
    assert_eq!(
        child.body(),
        format!("state=working:id=agent-00ff:title={}", b64("Find the bug"))
    );
    assert_eq!(
        Report::root(State::Idle, "  ").body(),
        "state=idle:app=zcode"
    );
}

/// 一行、没有控制字符：控制字符与分隔符换成空格，双向与零宽字符去掉，截在字符边界上。
#[test]
fn text_is_cleaned_and_cut_on_a_char_boundary() {
    assert_eq!(clean(" a\nb\tc\u{2028}d\u{7f}e ", MSG_MAX), "a b c d e");
    assert_eq!(clean("ab\u{202e}c\u{200b}d", MSG_MAX), "abcd");
    // “错” 是 3 个字节：上限 4 落在第二个字的中间，退到 3。
    assert_eq!(clean("错错", 4), "错");
    let long = "é".repeat(TITLE_MAX);
    let cut = clean(&long, TITLE_MAX);
    assert_eq!(cut.len(), TITLE_MAX);
    // 整段的上限：最长的 msg 与 title 编码后也在 4096 字节之内。
    let report = Report {
        id: Some(agent_id("a")),
        state: State::Blocked(Kind::Permission),
        title: "t".repeat(10 * TITLE_MAX),
        msg: "m".repeat(10 * MSG_MAX),
    };
    assert!(sequence(&report.body()).len() < 4096);
}

/// 根记录按顺序取第一个：询问、在跑、看到过结束的结局、其余 idle。
#[test]
fn the_root_follows_asks_turns_and_seen_outcomes() {
    let mut app = app();
    assert_eq!(root(&app).state, State::Idle);
    started(&mut app);
    assert_eq!(root(&app).state, State::Working);
    ask(&mut app, bash());
    let blocked = root(&app);
    assert_eq!(blocked.state, State::Blocked(Kind::Permission));
    assert_eq!(blocked.msg, "ZCode needs your permission: Bash");
    apply(
        &mut app,
        KernelEvent::InteractionResolved {
            interaction: InteractionId::new("i1"),
        },
    );
    assert_eq!(root(&app).state, State::Working);
    finished(&mut app, TurnOutcome::Success, true);
    assert_eq!(root_body(&app), "state=done:app=zcode");
    // 下一个回合开始：又是 working。
    started(&mut app);
    assert_eq!(root(&app).state, State::Working);
    // 中断：idle。
    finished(&mut app, TurnOutcome::Interrupted, true);
    assert_eq!(root(&app).state, State::Idle);
    // 失败：error，`msg` 是说明的第一行。
    started(&mut app);
    finished(
        &mut app,
        TurnOutcome::Failed {
            message: "quota exhausted\nretry later".into(),
        },
        true,
    );
    let error = root(&app);
    assert_eq!(error.state, State::Error);
    assert_eq!(error.msg, "quota exhausted");
}

/// 提问 `question`，计划审批 `permission`。
#[test]
fn asks_name_what_they_wait_for() {
    let mut app = app();
    ask(
        &mut app,
        InteractionRequest::Question {
            tool_use: ToolUseId::new("tu1"),
            questions: json!([]),
        },
    );
    assert_eq!(
        root_body(&app),
        format!(
            "state=blocked:kind=question:app=zcode:msg={}",
            b64("ZCode needs your input")
        )
    );
    let mut plan = self::app();
    ask(
        &mut plan,
        InteractionRequest::PlanApproval {
            tool_use: ToolUseId::new("tu1"),
            plan: "1. do it".into(),
        },
    );
    assert_eq!(root(&plan).state, State::Blocked(Kind::Permission));
}

/// 接上会话之后没看到 `TurnFinished`：旧会话上次的结局是 idle，不是 done / error。
#[test]
fn an_outcome_not_seen_is_idle() {
    let mut app = app();
    started(&mut app);
    finished(&mut app, TurnOutcome::Success, false);
    assert_eq!(root(&app).state, State::Idle);
    let mut failed = self::app();
    started(&mut failed);
    finished(
        &mut failed,
        TurnOutcome::Failed {
            message: "boom".into(),
        },
        true,
    );
    failed.program_status.attached();
    assert_eq!(root(&failed).state, State::Idle);
}

/// 在跑的后台子代理各一条子记录；后台命令、Monitor 与不在跑的子代理没有。
#[test]
fn running_background_agents_are_children() {
    let mut app = app();
    app.snapshots.background = vec![
        BackgroundEntry::new(
            "agent_1",
            BackgroundKind::Agent,
            "Find the bug",
            BackgroundStatus::Running,
        ),
        BackgroundEntry::new(
            "agent_2",
            BackgroundKind::Agent,
            "Done already",
            BackgroundStatus::Completed,
        ),
        BackgroundEntry::new(
            "shell_1",
            BackgroundKind::Shell,
            "npm run dev",
            BackgroundStatus::Running,
        ),
        BackgroundEntry::new(
            "mon_1",
            BackgroundKind::Monitor,
            "watch logs",
            BackgroundStatus::Running,
        ),
    ];
    let reports = wanted(&app);
    assert_eq!(reports.len(), 2);
    let child = &reports[1];
    assert_eq!(child.id.as_deref(), Some(agent_id("agent_1").as_str()));
    assert_eq!(child.state, State::Working);
    assert_eq!(child.title, "Find the bug");
    // id 满足协议的字符集与每段 32 字节。
    let id = agent_id("a-very-long-agent-id-0123456789abcdef-0123456789");
    assert!(id.len() <= 32);
    assert!(
        id.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.+-".contains(c))
    );
}

/// 只在变化时写；子记录离开时写 `clear:id=`；离开界面时写过才写一次不带 id 的 `clear`。
#[test]
fn bytes_are_written_only_on_change() {
    let mut status = ProgramStatus::default();
    assert_eq!(status.leave(), None);
    let idle = [Report::root(State::Idle, "")];
    assert_eq!(
        status.bytes(&idle).as_deref(),
        Some("\x1b]7501;state=idle:app=zcode\x1b\\")
    );
    assert_eq!(status.bytes(&idle), None);
    let child = Report {
        id: Some("agent-1".into()),
        state: State::Working,
        title: String::new(),
        msg: String::new(),
    };
    let both = [Report::root(State::Working, ""), child];
    assert_eq!(
        status.bytes(&both).as_deref(),
        Some("\x1b]7501;state=working:app=zcode\x1b\\\x1b]7501;state=working:id=agent-1\x1b\\")
    );
    assert_eq!(status.bytes(&both), None);
    let working = [Report::root(State::Working, "")];
    assert_eq!(
        status.bytes(&working).as_deref(),
        Some("\x1b]7501;state=clear:id=agent-1\x1b\\")
    );
    assert_eq!(
        status.leave().as_deref(),
        Some("\x1b]7501;state=clear\x1b\\")
    );
    assert_eq!(status.leave(), None);
}
