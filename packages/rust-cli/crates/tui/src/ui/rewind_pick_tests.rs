//! 挑选回退的点（docs/puffin-tui.md §4.2「回退（/rewind）」，`rewind_pick.rs`）：切线、变暗、`↶`、Also / Files、槽与按键。
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_kernel_api::ids::{MessageId, TurnId};
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{
    FileChange, KernelEvent, RewindOption, SequencedEvent, TurnOutcome,
};
use rust_cli_kernel_api::testing::selection;

use crate::ui::app::{Action, App};
use crate::ui::i18n::Lang;
use crate::ui::probe::{ColorLevel, Colors};
use crate::ui::rewind::Entry;
use crate::ui::transcript::Cache;
use crate::view::Transcript;

fn text(t: &str) -> Vec<ContentBlock> {
    vec![ContentBlock::Text { text: t.to_owned() }]
}

/// 三个回合的会话，回退的点是三条你的消息；第一条之后改过文件。
fn app(lang: Lang) -> App {
    let mut view = Transcript::new(
        selection(),
        PermissionMode::Default,
        crate::view::test_workspace(),
    );
    let mut seq = 0;
    let mut apply = |view: &mut Transcript, event: KernelEvent| {
        seq += 1;
        view.apply(
            SequencedEvent {
                seq,
                revision: 0,
                event,
            },
            0,
        );
    };
    for n in 0..3 {
        apply(
            &mut view,
            KernelEvent::TurnStarted {
                turn: TurnId::new(format!("t{n}")),
                command: None,
            },
        );
        apply(
            &mut view,
            KernelEvent::MessageDone {
                message: Message::User(UserMessage {
                    id: MessageId::new(format!("u{n}")),
                    content: text(&format!("Question {n}")),
                    origin: UserOrigin::Human,
                }),
            },
        );
        apply(
            &mut view,
            KernelEvent::MessageDone {
                message: Message::Assistant(AssistantMessage {
                    id: MessageId::new(format!("a{n}")),
                    content: text(&format!("Answer {n}")),
                    model: "scripted".into(),
                    stop_reason: None,
                    usage: None,
                }),
            },
        );
        apply(
            &mut view,
            KernelEvent::TurnFinished {
                turn: TurnId::new(format!("t{n}")),
                outcome: TurnOutcome::Success,
                usage: None,
            },
        );
    }
    let mut app = App::new(
        Colors::new(ColorLevel::TrueColor, Some((21, 22, 26)))
            .with_foreground(Some((214, 216, 220))),
        lang,
        view,
        Some("/Users/dev".into()),
        "0.1.0".into(),
    );
    app.session = Some("s".into());
    app.rewind = (0..3)
        .map(|n| Entry {
            option: RewindOption {
                message: MessageId::new(format!("u{n}")),
                files: if n == 1 {
                    vec![FileChange {
                        path: "src/footer.rs".into(),
                        additions: 12,
                        deletions: 4,
                        machine: Default::default(),
                    }]
                } else {
                    Vec::new()
                },
                code: true,
            },
            text: format!("Question {n}"),
            // 上面的回答（`rewind::entries_of` 的规则）：第一条没有。
            answer: (n > 0).then(|| MessageId::new(format!("a{}", n - 1))),
        })
        .collect();
    app
}

fn draw(app: &mut App) -> Vec<rust_cli_tui_kit::render::StyledLine> {
    let mut cache = Cache::default();
    let now = app.started + Duration::from_secs(5);
    crate::ui::draw::draw(app, &mut cache, now, 0, 100, 40).lines
}

fn screen(app: &mut App) -> Vec<String> {
    draw(app)
        .iter()
        .map(|line| line.plain().trim_end().to_owned())
        .collect()
}

fn row<'a>(lines: &'a [String], needle: &str) -> Option<&'a String> {
    lines.iter().find(|l| l.contains(needle))
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn picking_draws_the_cut_the_chevron_and_the_slot() {
    let mut app = app(Lang::En);
    app.pick_point(1);
    let lines = screen(&mut app);
    let cut = row(&lines, "↶ Back to before your 2nd message").expect("the cut line");
    assert!(
        cut.ends_with("the 2 turns below stay behind, in /resume ──"),
        "{lines:#?}"
    );
    // 切线在你的第 2 条消息之前：第 1 个回答在它上面，第 2 条消息在它下面。
    let at = |needle: &str| lines.iter().position(|l| l.contains(needle)).unwrap();
    let cut_at = at("↶ Back to");
    assert!(
        at("Answer 0") < cut_at && cut_at < at("Question 1"),
        "{lines:#?}"
    );
    // 上下各空一行。
    assert!(
        lines[cut_at - 1].is_empty() && lines[cut_at + 1].is_empty(),
        "{lines:#?}"
    );
    // 输入框：`↶` 加这个点的消息；Files 一行；槽是 `↑↓ point`。
    let input = row(&lines, " ↶ Question 1").expect("the draft");
    assert!(input.ends_with("↑↓ point"), "{lines:#?}");
    let files = row(&lines, "Files   stay as they are now").expect("the Files row");
    assert!(
        files.ends_with("the 2 turns below changed +12 −4"),
        "{lines:#?}"
    );
}

#[test]
fn what_stays_behind_is_dimmed() {
    let mut app = app(Lang::En);
    let ink = |lines: &[rust_cli_tui_kit::render::StyledLine], needle: &str| {
        let line = lines.iter().find(|l| l.plain().contains(needle)).unwrap();
        let span = line.spans.iter().find(|s| s.text.contains(needle)).unwrap();
        span.style
    };
    let before = draw(&mut app);
    app.pick_point(1);
    let after = draw(&mut app);
    // 点上面的照常，点和下面的变暗。
    assert_eq!(ink(&before, "Answer 0"), ink(&after, "Answer 0"));
    assert_ne!(ink(&before, "Answer 1"), ink(&after, "Answer 1"));
    assert_ne!(ink(&before, "Answer 2"), ink(&after, "Answer 2"));
}

#[test]
fn arrows_change_the_point_and_esc_leaves() {
    let mut app = app(Lang::En);
    app.pick_point(2);
    let lines = screen(&mut app);
    assert!(
        row(&lines, "↶ Back to before your last message").is_some(),
        "{lines:#?}"
    );
    assert!(row(&lines, "↑ earlier").is_some(), "{lines:#?}");
    // 改过的草稿换走再换回来还在。
    app.on_key(key(KeyCode::Char('!')), app.started);
    app.on_key(key(KeyCode::Up), app.started);
    let lines = screen(&mut app);
    assert!(row(&lines, " ↶ Question 1").is_some(), "{lines:#?}");
    app.on_key(key(KeyCode::Up), app.started);
    let lines = screen(&mut app);
    let cut = row(&lines, "↶ Back to the start").expect("the first point");
    assert!(
        cut.contains("all 3 turns stay behind, in /resume"),
        "{lines:#?}"
    );
    assert!(row(&lines, "↓ later").is_some(), "{lines:#?}");
    // 到头了：槽里说一句，到下一个键为止。
    app.on_key(key(KeyCode::Up), app.started);
    let lines = screen(&mut app);
    assert!(
        row(&lines, "This is your first message.").is_some(),
        "{lines:#?}"
    );
    app.on_key(key(KeyCode::Down), app.started);
    app.on_key(key(KeyCode::Down), app.started);
    let lines = screen(&mut app);
    // `!` 是普通的字，不进 shell 模式。
    assert!(row(&lines, " ↶ Question 2!").is_some(), "{lines:#?}");
    app.on_key(key(KeyCode::Esc), app.started);
    let lines = screen(&mut app);
    assert!(row(&lines, "↶").is_none(), "{lines:#?}");
    assert!(row(&lines, " ›  Send a message").is_some(), "{lines:#?}");
}

#[test]
fn an_empty_draft_goes_back_only() {
    let mut app = app(Lang::En);
    app.pick_point(1);
    while !app.composer.is_empty() {
        app.on_key(key(KeyCode::Backspace), app.started);
    }
    let lines = screen(&mut app);
    let input = row(&lines, " ↶  Nothing will be sent").expect("the placeholder");
    assert!(input.ends_with("⏎ go back only"), "{lines:#?}");
}

#[test]
fn chinese_uses_the_translations() {
    let mut app = app(Lang::Zh);
    app.pick_point(1);
    let lines = screen(&mut app);
    let cut = row(&lines, "↶ 回到你第 2 条消息之前").expect("the cut line");
    assert!(
        cut.contains("下面的 2 个回合留在后面，在 /resume 里"),
        "{lines:#?}"
    );
    assert!(
        row(&lines, "文件    保持现在的样子").is_some(),
        "{lines:#?}"
    );
    assert!(row(&lines, "↑↓ 换点").is_some(), "{lines:#?}");
}

#[test]
fn enter_forks_on_the_answer_above_and_a_refusal_comes_back() {
    let mut app = app(Lang::En);
    app.view.apply(
        SequencedEvent {
            seq: 100,
            revision: 0,
            event: KernelEvent::TitleChanged {
                title: "Footer work".into(),
            },
        },
        0,
    );
    app.pick_point(1);
    let action = app.on_key(key(KeyCode::Enter), app.started);
    assert!(
        matches!(&action, Some(Action::RewindFork { message }) if message.as_str() == "u1"),
        "{action:?}"
    );
    // fork 中：键都停着。
    assert!(app.on_key(key(KeyCode::Up), app.started).is_none());
    assert_eq!(app.picking_point(), Some(1));
    // 拒绝：回到挑选，草稿还在，输入框上方说一句。
    app.rewound(Err("no".into()));
    let lines = screen(&mut app);
    assert!(
        row(
            &lines,
            "Couldn't go back: the agent did not start the new session."
        )
        .is_some(),
        "{lines:#?}"
    );
    assert!(row(&lines, " ↶ Question 1").is_some(), "{lines:#?}");
    // 开出了新会话：它的视图到了时挑选结束，痕迹行说出离开的会话，草稿作为第一条消息发出。
    app.on_key(key(KeyCode::Enter), app.started);
    app.rewound(Ok("s2".into()));
    // 新会话的视图（runner 切过去之后）：fork 出来的，`meta.parent` 是离开的会话，第一行是痕迹行。
    app.view = Transcript::from_view(forked_view(), 0);
    let send = app.rewound_into();
    assert!(
        matches!(&send, Some(Action::Submit { text, .. }) if text == "Question 1"),
        "{send:?}"
    );
    let lines = screen(&mut app);
    assert!(row(&lines, "↶ Back to").is_none(), "{lines:#?}");
    assert!(
        row(
            &lines,
            " ↶ Rewound here  “Footer work” is in /resume, as it was"
        )
        .is_some(),
        "{lines:#?}"
    );
    // 之后 `/resume` 回到它：痕迹行还在，这次运行不知道离开的会话的标题时只写 `Rewound here`。
    app.view = Transcript::from_view(forked_view(), 0);
    let lines = screen(&mut app);
    let trace = row(&lines, " ↶ Rewound here").expect("the trace row");
    assert!(!trace.contains("/resume"), "{lines:#?}");
}

/// fork 出的会话的快照：复制来的一个回合。
fn forked_view() -> rust_cli_kernel_api::session_api::SessionView {
    use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
    use rust_cli_kernel_api::ids::{EnvironmentId, SessionId, TraceId};
    use rust_cli_kernel_api::store::{SessionKind, SessionMeta};
    rust_cli_kernel_api::session_api::SessionView::new(
        SessionMeta {
            id: SessionId::new("s2"),
            kind: SessionKind::Fork,
            parent: Some(SessionId::new("s")),
            workspace: WorkspaceRef {
                environment: EnvironmentId::new("local"),
                root: EnvPath::new("/Users/dev/project"),
            },
            title: "Footer work (fork)".into(),
            trace: TraceId::new("s"),
            created_at_ms: 0,
        },
        vec![
            Message::User(UserMessage {
                id: MessageId::new("c0"),
                content: text("Question 0"),
                origin: UserOrigin::Human,
            }),
            Message::Assistant(AssistantMessage {
                id: MessageId::new("ca0"),
                content: text("Answer 0"),
                model: "scripted".into(),
                stop_reason: None,
                usage: None,
            }),
        ],
        selection(),
        PermissionMode::Default,
    )
}

#[test]
fn the_first_point_starts_a_new_session_with_the_draft() {
    let mut app = app(Lang::En);
    app.pick_point(0);
    let action = app.on_key(key(KeyCode::Enter), app.started);
    match action {
        Some(Action::Many(actions)) => {
            assert!(matches!(actions[0], Action::Clear), "{actions:?}");
            assert!(
                matches!(&actions[1], Action::Submit { text, .. } if text == "Question 0"),
                "{actions:?}"
            );
        }
        other => panic!("{other:?}"),
    }
    assert!(app.picking.is_none());
}

#[test]
fn a_click_on_one_of_your_messages_moves_the_point() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let mut app = app(Lang::En);
    app.pick_point(2);
    let lines = screen(&mut app);
    let at = lines.iter().position(|l| l.contains("Question 0")).unwrap();
    app.on_mouse(
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: u16::try_from(at).unwrap(),
            modifiers: KeyModifiers::NONE,
        },
        app.started,
    );
    assert_eq!(app.picking_point(), Some(0));
    assert!(app.pointer.selection.is_none());
    let lines = screen(&mut app);
    assert!(row(&lines, "↶ Back to the start").is_some(), "{lines:#?}");
}

/// 有草稿时 esc 什么也不做（Puffin：草稿不会被换掉）；空的输入框上第一次说 `esc rewind`。以前有草稿时连按两次清空。
#[test]
fn esc_leaves_a_draft_alone() {
    let mut app = app(Lang::En);
    for c in "keep me".chars() {
        app.on_key(key(KeyCode::Char(c)), app.started);
    }
    app.on_key(key(KeyCode::Esc), app.started);
    app.on_key(key(KeyCode::Esc), app.started);
    let lines = screen(&mut app);
    assert!(row(&lines, " › keep me").is_some(), "{lines:#?}");
    assert!(app.picking.is_none());
    assert!(row(&lines, "again to clear").is_none(), "{lines:#?}");
    while !app.composer.is_empty() {
        app.on_key(key(KeyCode::Backspace), app.started);
    }
    app.on_key(key(KeyCode::Esc), app.started);
    let lines = screen(&mut app);
    assert!(row(&lines, "esc rewind").is_some(), "{lines:#?}");
}

/// 挑选回退的点时 ctrl+n 是下一个点，回到切线是 ctrl+End（tui-kit.md §4.0）。
#[test]
fn while_picking_ctrl_n_steps_and_ctrl_end_returns_to_the_cut() {
    let mut app = app(Lang::En);
    app.pick_point(0);
    let now = app.started;
    let press = |app: &mut App, code| {
        app.on_key(KeyEvent::new(code, KeyModifiers::CONTROL), now);
    };
    press(&mut app, KeyCode::Char('n'));
    assert_eq!(app.picking_point(), Some(1));
    if let Some(picking) = app.picking.as_mut() {
        picking.at_cut = false;
    }
    app.port.scroll = 7;
    press(&mut app, KeyCode::End);
    assert_eq!(app.port.scroll, 0);
    assert!(app.picking.as_ref().is_some_and(|p| p.at_cut));
}
