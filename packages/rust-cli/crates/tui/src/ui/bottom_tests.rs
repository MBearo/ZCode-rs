//! 排队的消息下面的立即发送提示（docs/specs/rust-cli/tui.md §5.1.1，修复线 F104）：原文、位置、键的选择与
//! 不显示的情形。旧代码没有这一行，这些断言都失败。修复线 F105：整行一种次要色。
use ratatui::style::Modifier;
use rust_cli_kernel_api::ids::{CommandId, TurnId};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{
    Delivery, KernelEvent, QueuedInput, SequencedEvent, TurnOutcome,
};

use crate::ui::app::App;
use crate::ui::i18n::Lang;
use crate::ui::palette::{ColorLevel, Palette};
use crate::view::Transcript;

const WIDTH: usize = 80;

fn selection() -> ModelSelection {
    ModelSelection {
        provider: "fixture".into(),
        model: "scripted".into(),
        reasoning: None,
        connection: None,
    }
}

fn app(lang: Lang) -> App {
    App::new(
        Palette::new(ColorLevel::TrueColor, Some((0, 0, 0))),
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
            delivery: Delivery::Steer,
        }],
    }
}

/// 画出来的队列区域，一行一个字符串。
fn lines(app: &App) -> Vec<String> {
    super::queue(app, WIDTH)
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect()
}

#[test]
fn the_key_is_shown_under_the_queued_input_while_the_turn_runs() {
    let mut app = app(Lang::En);
    feed(&mut app, vec![started(), queued("also check the date")]);
    assert_eq!(
        lines(&app),
        [
            "❯ also check the date".to_owned(),
            "  ctrl+x ctrl+s to send now".to_owned(),
        ]
    );
}

/// 修复线 F105（tui.md §5.1.1，用户要求整行灰）：缩进之后整行是一段 `secondary`，键不加粗。旧代码按按键提示组件画，
/// 键是单独一段 `key` 样式。
#[test]
fn the_whole_line_is_secondary() {
    for enhanced in [false, true] {
        let mut app = app(Lang::En);
        app.keyboard_enhanced = enhanced;
        feed(&mut app, vec![started(), queued("also check the date")]);
        let drawn = super::queue(&app, WIDTH);
        let hint = drawn.last().expect("the hint line");
        let spans: Vec<(&str, _)> = hint
            .spans
            .iter()
            .map(|span| (span.content.as_ref(), span.style))
            .collect();
        let key = if enhanced {
            "ctrl+enter"
        } else {
            "ctrl+x ctrl+s"
        };
        assert_eq!(
            spans,
            [(
                format!("  {key} to send now").as_str(),
                app.palette.secondary()
            )],
        );
        assert!(
            !hint
                .spans
                .iter()
                .any(|span| span.style.add_modifier.contains(Modifier::BOLD)),
            "{hint:?}"
        );
    }
}

/// 中文界面：`<键> 立即发送`（与 footer 快捷键面板一致）。
#[test]
fn the_chinese_line_says_send_now() {
    let mut app = app(Lang::Zh);
    feed(&mut app, vec![started(), queued("再看看日期")]);
    assert_eq!(
        lines(&app).last().map(String::as_str),
        Some("  ctrl+x ctrl+s 立即发送")
    );
}

/// 终端分辨得出 ctrl+enter（键盘增强）时写 ctrl+enter。
#[test]
fn an_enhanced_terminal_shows_ctrl_enter() {
    let mut app = app(Lang::En);
    app.keyboard_enhanced = true;
    feed(&mut app, vec![started(), queued("also check the date")]);
    assert_eq!(
        lines(&app).last().map(String::as_str),
        Some("  ctrl+enter to send now")
    );
    app.lang = Lang::Zh;
    app.strings = crate::ui::i18n::strings(Lang::Zh);
    assert_eq!(
        lines(&app).last().map(String::as_str),
        Some("  ctrl+enter 立即发送")
    );
}

/// 空闲时、队列空时、回合结束之后都不显示。
#[test]
fn the_key_is_not_shown_without_a_running_turn_or_a_queued_input() {
    let mut idle = app(Lang::En);
    feed(&mut idle, vec![queued("also check the date")]);
    assert_eq!(lines(&idle), ["❯ also check the date".to_owned()]);

    let mut running = app(Lang::En);
    feed(&mut running, vec![started()]);
    assert!(lines(&running).is_empty(), "no queue, no hint");

    let mut done = app(Lang::En);
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

/// 只读查看子代理时不显示（对照实现在这个视图里整个排队列表都不显示）。
#[test]
fn the_key_is_not_shown_while_viewing_a_subagent() {
    let mut app = app(Lang::En);
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
    assert_eq!(lines(&app), ["❯ also check the date".to_owned()]);
}
