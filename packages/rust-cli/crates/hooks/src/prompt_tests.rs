//! prompt hook（spec §5.3）与 `/goal` 的停止条件（goal.md §5）。

use rust_cli_kernel_api::message::{Message, UserOrigin};
use rust_cli_kernel_api::model::OutputFormat;
use rust_cli_kernel_api::testing::{ScriptedModel, SequentialIds, selection, text_response};
use serde_json::json;

use super::*;

fn hook(prompt: &str, continue_on_block: bool) -> PromptHook {
    PromptHook {
        prompt: prompt.to_owned(),
        model: None,
        timeout: None,
        condition: None,
        continue_on_block,
    }
}

async fn evaluate_hook(model: &ScriptedModel, hook: &PromptHook, stop_event: bool) -> HookResult {
    let default = selection();
    let (turn, agent) = (RunId::new("run_hook"), AgentId::new("agent_hook"));
    run(
        PromptRun {
            requester: Requester {
                session: &SessionId::new("sess_hook"),
                run: Some(&turn),
                agent: Some(&agent),
            },
            model: Some(model),
            default_model: Some(&default),
            hook,
            input: "{\"x\":1}",
            timeout_ms: 5_000,
            stop_event,
        },
        CancellationToken::new(),
    )
    .await
}

fn text(message: &ApiMessage, index: usize) -> &str {
    match &message.content[index].block {
        ContentBlock::Text { text } => text,
        other => panic!("not text: {other:?}"),
    }
}

#[test]
fn arguments_placeholder_fence_and_truncation() {
    assert_eq!(with_arguments("check $ARGUMENTS now", "{}"), "check {} now");
    assert_eq!(with_arguments("check", "{}"), "check\n\nARGUMENTS: {}");
    assert_eq!(
        with_arguments("keep \\$ARGUMENTS", "{}"),
        "keep $ARGUMENTS\n\nARGUMENTS: {}"
    );
    assert_eq!(strip_fence("```json\n{\"ok\":true}\n```"), "{\"ok\":true}");
    assert_eq!(truncate("abc", 3), "abc");
    assert_eq!(truncate("abcdef", 4), "abcd\u{2026} [+2 chars]");
    // 代理对算两个码元，不拆开。
    assert_eq!(truncate("a😀b", 2), "a\u{2026} [+3 chars]");
}

#[tokio::test]
async fn sends_the_evaluation_request() {
    let model = ScriptedModel::new(vec![text_response(&[
        "{\"ok\": true, \"reason\": \"fine\"}",
    ])]);
    let result = evaluate_hook(&model, &hook("Is $ARGUMENTS safe?", false), false).await;
    assert_eq!(result.outcome, Outcome::Success);
    let request = model.requests().remove(0);
    // 工作线 F52：请求带上 hook 所在的会话（`metadata.user_id` 的 `session_id`，model.md §3.3）。
    assert_eq!(
        request.options.session_id,
        Some(SessionId::new("sess_hook"))
    );
    // 工作线 F64：触发 hook 的回合与子代理（每请求头的 `x-query-id`、`x-zcode-session-type`，model.md §2.4）。
    assert_eq!(request.options.run, Some(RunId::new("run_hook")));
    assert_eq!(request.options.agent, Some(AgentId::new("agent_hook")));
    assert_eq!(request.system.len(), 2);
    assert_eq!(request.system[0].text, identity());
    assert!(
        request.system[1]
            .text
            .starts_with("You are evaluating a hook condition in ZCode.")
    );
    assert!(
        request
            .system
            .iter()
            .all(|block| block.cache_control.is_some())
    );
    assert_eq!(request.thinking, None);
    assert_eq!(request.source, QuerySource::Side("hook_prompt".to_owned()));
    assert!(request.tools.is_empty());
    assert!(matches!(
        request.options.output_format,
        Some(OutputFormat::JsonSchema { .. })
    ));
    assert_eq!(text(&request.messages[0], 0), "Is {\"x\":1} safe?");
    assert!(request.messages[0].content[0].cache_control.is_some());
}

#[tokio::test]
async fn not_met_blocks_and_stops_unless_continue_on_block() {
    let model = ScriptedModel::new(vec![
        text_response(&["```json\n{\"ok\": false, \"reason\": \"tests fail\"}\n```"]),
        text_response(&["{\"ok\": false}"]),
    ]);
    let blocked = evaluate_hook(&model, &hook("Tests pass?", false), false).await;
    assert_eq!(blocked.outcome, Outcome::Blocking);
    assert_eq!(blocked.blocking.unwrap().error, "[Tests pass?]: tests fail");
    assert!(blocked.prevent);
    assert_eq!(blocked.stop_reason.as_deref(), Some("tests fail"));
    let continued = evaluate_hook(&model, &hook("Tests pass?", true), false).await;
    assert!(!continued.prevent);
    assert_eq!(
        continued.blocking.unwrap().error,
        "[Tests pass?]: undefined"
    );
}

#[tokio::test]
async fn bad_responses_and_unsupported_cases() {
    let model = ScriptedModel::new(vec![
        text_response(&["not json"]),
        text_response(&["{\"reason\": 1}"]),
    ]);
    assert_eq!(
        evaluate_hook(&model, &hook("p", false), false)
            .await
            .error
            .as_deref(),
        Some("JSON validation failed")
    );
    assert!(
        evaluate_hook(&model, &hook("p", false), false)
            .await
            .error
            .unwrap()
            .starts_with("Schema validation failed")
    );
    let stop = evaluate_hook(&model, &hook("p", false), true).await;
    assert_eq!(stop.outcome, Outcome::NonBlockingError);
    let missing = run(
        PromptRun {
            requester: Requester {
                session: &SessionId::new("sess_hook"),
                run: None,
                agent: None,
            },
            model: None,
            default_model: None,
            hook: &hook("p", false),
            input: "{}",
            timeout_ms: 1,
            stop_event: false,
        },
        CancellationToken::new(),
    )
    .await;
    assert_eq!(
        missing.error.as_deref(),
        Some("Error executing prompt hook: no model is configured for prompt hooks")
    );
}

fn transcript() -> Vec<ApiMessage> {
    let content = |text: &str| ApiContent {
        block: ContentBlock::Text {
            text: text.to_owned(),
        },
        cache_control: ephemeral(),
    };
    vec![
        ApiMessage {
            role: ApiRole::User,
            content: vec![content("/goal tests pass")],
            output_config: None,
        },
        ApiMessage {
            role: ApiRole::Assistant,
            content: vec![content("Working on it.")],
            output_config: None,
        },
    ]
}

async fn stop(model: &ScriptedModel, transcript: Vec<ApiMessage>) -> StopVerdict {
    let default = selection();
    let ids = SequentialIds::default();
    evaluate(
        StopRun {
            requester: Requester {
                session: &SessionId::new("sess_hook"),
                run: None,
                agent: None,
            },
            model: Some(model),
            default_model: Some(&default),
            ids: &ids,
            condition: "tests pass",
            input: "{\"hook_event_name\":\"Stop\"}",
            transcript,
            timeout_ms: 5_000,
        },
        CancellationToken::new(),
    )
    .await
}

#[tokio::test]
async fn the_stop_condition_reads_the_transcript() {
    let model = ScriptedModel::new(vec![text_response(&[
        "{\"ok\": true, \"reason\": \"all green\"}",
    ])]);
    let verdict = stop(&model, transcript()).await;
    assert_eq!(
        verdict,
        StopVerdict::Met {
            reason: Some("all green".to_owned())
        }
    );
    let request = model.requests().remove(0);
    assert_eq!(
        request.options.session_id,
        Some(SessionId::new("sess_hook"))
    );
    assert!(
        request.system[1]
            .text
            .starts_with("You are evaluating a stop-condition hook in ZCode.")
    );
    // 对话记录在前，断点只留在最后一个块上。
    assert_eq!(request.messages.len(), 3);
    assert!(
        request.messages[..2]
            .iter()
            .flat_map(|m| &m.content)
            .all(|c| c.cache_control.is_none())
    );
    let last = request.messages.last().unwrap();
    assert_eq!(last.role, ApiRole::User);
    assert_eq!(
        text(last, 0),
        "Based on the conversation transcript above, has the following stopping condition been satisfied? Answer based on transcript evidence only.\n\nCondition: tests pass\n\nARGUMENTS: {\"hook_event_name\":\"Stop\"}"
    );
    assert!(last.content[0].cache_control.is_some());
    let format = serde_json::to_value(&request.options.output_format).unwrap();
    assert_eq!(format["schema"]["required"], json!(["ok", "reason"]));
}

#[tokio::test]
async fn the_four_stop_answers() {
    let model = ScriptedModel::new(vec![
        text_response(&["{\"ok\": false, \"reason\": \"red\"}"]),
        text_response(&["{\"ok\": false, \"impossible\": true, \"reason\": \"no tests\"}"]),
        text_response(&["oops"]),
    ]);
    let StopVerdict::NotMet { reason, feedback } = stop(&model, transcript()).await else {
        panic!("not met expected");
    };
    assert_eq!(reason.as_deref(), Some("red"));
    let Message::User(user) = feedback else {
        panic!("meta feedback expected")
    };
    assert_eq!(
        user.origin,
        UserOrigin::Meta {
            source: "stop_hook_feedback".to_owned()
        }
    );
    assert_eq!(
        user.content,
        vec![ContentBlock::Text {
            text: "Stop hook feedback:\n[tests pass]: red".to_owned()
        }]
    );
    assert_eq!(
        stop(&model, transcript()).await,
        StopVerdict::Impossible {
            reason: Some("no tests".to_owned())
        }
    );
    assert_eq!(
        stop(&model, transcript()).await,
        StopVerdict::Failed {
            message: "JSON validation failed".to_owned()
        }
    );
}

#[tokio::test]
async fn a_repeated_condition_is_truncated_in_the_feedback() {
    let long = "x".repeat(600);
    let model = ScriptedModel::new(vec![text_response(&[
        "{\"ok\": false, \"reason\": \"no\"}",
    ])]);
    let default = selection();
    let ids = SequentialIds::default();
    let mut history = transcript();
    history.push(ApiMessage {
        role: ApiRole::User,
        content: vec![ApiContent {
            block: ContentBlock::Text {
                text: format!("Stop hook feedback:\n[{long}]: no"),
            },
            cache_control: None,
        }],
        output_config: None,
    });
    let verdict = evaluate(
        StopRun {
            requester: Requester {
                session: &SessionId::new("sess_hook"),
                run: None,
                agent: None,
            },
            model: Some(&model),
            default_model: Some(&default),
            ids: &ids,
            condition: &long,
            input: "{}",
            transcript: history,
            timeout_ms: 5_000,
        },
        CancellationToken::new(),
    )
    .await;
    let StopVerdict::NotMet { feedback, .. } = verdict else {
        panic!("not met expected");
    };
    let Message::User(user) = feedback else {
        panic!("meta feedback expected")
    };
    let ContentBlock::Text { text } = &user.content[0] else {
        panic!("text expected")
    };
    assert_eq!(
        text,
        &format!(
            "Stop hook feedback:\n[{}\u{2026} [+100 chars]]: no",
            "x".repeat(500)
        )
    );
}
