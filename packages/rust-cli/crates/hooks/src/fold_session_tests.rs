//! 会话类事件与“外部”路径的合并（spec §7）。

use rust_cli_kernel_api::message::{ContentBlock, Message, UserOrigin};
use rust_cli_kernel_api::testing::SequentialIds;
use serde_json::json;

use super::*;

fn text(message: &Message) -> String {
    match message {
        Message::Attachment(attachment) => attachment.text.clone(),
        Message::User(user) => match &user.content[0] {
            ContentBlock::Text { text } => text.clone(),
            _ => unreachable!(),
        },
        _ => unreachable!(),
    }
}

fn blocking(error: &str) -> HookResult {
    HookResult::blocking(error.to_owned(), "g".to_owned())
}

fn context(text: &str) -> HookResult {
    HookResult {
        additional_context: Some(text.to_owned()),
        ..HookResult::default()
    }
}

#[test]
fn stop_feedback_context_and_prevent() {
    let ids = SequentialIds::default();
    let fold = Fold {
        ids: &ids,
        event: "Stop",
        hook_name: "Stop",
    };
    let out = stop(&fold, &[blocking("[g]: run tests"), context("note")]);
    assert_eq!(out.messages.len(), 2);
    assert_eq!(
        text(&out.messages[0]),
        "Stop hook feedback:\n[g]: run tests"
    );
    assert!(matches!(&out.messages[0], Message::User(user)
        if user.origin == UserOrigin::Meta { source: "stop_hook_feedback".to_owned() }));
    assert!(text(&out.messages[1]).contains("Stop hook additional context: note"));
    assert!(!out.prevent_continuation);
    let prevented = stop(
        &fold,
        &[HookResult {
            prevent: true,
            ..HookResult::default()
        }],
    );
    assert!(prevented.prevent_continuation);
    assert!(
        text(&prevented.messages[0])
            .contains("Stop hook stopped continuation: Stop hook prevented continuation")
    );
    let sub = Fold {
        ids: &ids,
        event: "SubagentStop",
        hook_name: "SubagentStop:Explore",
    };
    assert!(
        text(&stop(&sub, &[context("c")]).messages[0])
            .contains("SubagentStop hook additional context: c")
    );
}

#[test]
fn user_prompt_submit_paths() {
    let ids = SequentialIds::default();
    let fold = Fold {
        ids: &ids,
        event: "UserPromptSubmit",
        hook_name: "UserPromptSubmit",
    };
    let blocked = user_prompt_submit(
        &fold,
        &[context("early"), blocking("[g]: no secrets")],
        "my prompt",
    );
    assert_eq!(blocked.blocking_error.as_deref(), Some("[g]: no secrets"));
    assert!(blocked.messages.is_empty());
    assert_eq!(
        blocked.notices[0],
        "UserPromptSubmit operation blocked by hook:\n[g]: no secrets\n\nOriginal prompt: my prompt"
    );
    let stopped = user_prompt_submit(
        &fold,
        &[HookResult {
            prevent: true,
            stop_reason: Some("later".to_owned()),
            ..HookResult::default()
        }],
        "p",
    );
    assert!(stopped.prevent_continuation);
    assert_eq!(
        text(&stopped.messages[0]),
        "Operation stopped by hook: later"
    );
    let ok = user_prompt_submit(
        &fold,
        &[
            HookResult {
                success_content: Some("today is friday".to_owned()),
                session_title: Some("t".to_owned()),
                ..HookResult::default()
            },
            HookResult {
                success_content: Some(String::new()),
                ..context("ctx")
            },
        ],
        "p",
    );
    assert_eq!(ok.messages.len(), 2);
    assert!(text(&ok.messages[0]).contains("UserPromptSubmit hook success: today is friday"));
    assert!(text(&ok.messages[1]).contains("UserPromptSubmit hook additional context: ctx"));
    assert_eq!(ok.session_title.as_deref(), Some("t"));
}

#[test]
fn session_start_merges_contexts_last() {
    let ids = SequentialIds::default();
    let fold = Fold {
        ids: &ids,
        event: "SessionStart",
        hook_name: "SessionStart:startup",
    };
    let out = session_start(
        &fold,
        &[
            context("a"),
            HookResult {
                success_content: Some("plain".to_owned()),
                initial_user_message: Some("go".to_owned()),
                watch_paths: vec!["/w/x".to_owned()],
                ..HookResult::default()
            },
            context("b"),
        ],
    );
    assert_eq!(out.messages.len(), 2);
    assert!(text(&out.messages[0]).contains("SessionStart:startup hook success: plain"));
    assert!(text(&out.messages[1]).contains("SessionStart hook additional context: a\nb"));
    assert_eq!(out.initial_user_message.as_deref(), Some("go"));
    assert_eq!(out.watch_paths, ["/w/x"]);
}

fn summary(command: &str, succeeded: bool, output: &str, blocked: bool) -> Summary {
    Summary {
        command: command.to_owned(),
        succeeded,
        output: output.to_owned(),
        blocked,
        cancelled: false,
    }
}

#[test]
fn outside_events() {
    let summaries = [
        summary("a", true, " focus on tests \n", false),
        summary("b", true, "", false),
        summary("c", false, "boom", false),
        summary("d", false, "stop", true),
        Summary {
            cancelled: true,
            ..summary("e", false, "Hook cancelled", false)
        },
    ];
    let pre = outside(HookEventName::PreCompact, &summaries);
    assert_eq!(pre.custom_instructions.as_deref(), Some("focus on tests"));
    assert_eq!(
        pre.notices[0],
        "PreCompact [a] completed successfully: focus on tests\nPreCompact [b] completed successfully\nPreCompact [c] failed: boom\nPreCompact [d] failed: stop"
    );
    assert_eq!(pre.blocking_error.as_deref(), Some("[d]: stop"));
    let post = outside(HookEventName::PostCompact, &summaries[..2]);
    assert_eq!(
        post.notices[0],
        "PostCompact [a] completed successfully: focus on tests\nPostCompact [b] completed successfully"
    );
    let end = outside(HookEventName::SessionEnd, &summaries);
    assert_eq!(
        end.notices,
        [
            "SessionEnd hook [c] failed: boom",
            "SessionEnd hook [d] failed: stop",
            "SessionEnd hook [e] failed: Hook cancelled"
        ]
    );
    assert_eq!(prompt_of(&json!({"prompt": "hi"})), "hi");
}
