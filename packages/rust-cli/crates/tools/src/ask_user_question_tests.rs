use std::collections::BTreeMap;
use std::sync::Arc;

use rust_cli_kernel_api::orchestration::{InteractionRequest, QuestionAnswer};
use rust_cli_kernel_api::permission::{PermissionDecision, PermissionMode};
use rust_cli_kernel_api::testing::ScriptedInteractions;
use rust_cli_kernel_api::tool::{Tool, Validation};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::*;
use crate::test_more::{Ctx, output_text};

fn input() -> Value {
    json!({"questions": [
        {"question": "Which library?", "header": "Library", "multiSelect": false, "options": [
            {"label": "date-fns", "description": "small"},
            {"label": "dayjs", "description": "tiny"}]},
        {"question": "Which features?", "header": "Features", "multiSelect": true, "options": [
            {"label": "Auth", "description": "a"},
            {"label": "Logs, metrics", "description": "b"},
            {"label": "Cache", "description": "c"}]}
    ]})
}

fn answers(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(question, answer)| ((*question).to_owned(), (*answer).to_owned()))
        .collect()
}

/// 只有答案的结果文本。
fn plain(input: &Value, answers: &BTreeMap<String, String>) -> String {
    result_text(
        input,
        &Answers {
            answers,
            annotations: &BTreeMap::new(),
            response: None,
            follow_up: false,
            afk_timeout_ms: None,
        },
    )
}

#[test]
fn answers_inside_the_options_read_as_answered() {
    let text = plain(
        &input(),
        &answers(&[
            ("Which library?", "dayjs"),
            ("Which features?", "Auth, \"Logs, metrics\""),
        ]),
    );
    assert_eq!(
        text,
        "Your questions have been answered: \"Which library?\"=\"dayjs\", \"Which features?\"=\"Auth, \"Logs, metrics\"\". You can now continue with these answers in mind."
    );
}

#[test]
fn free_text_answers_ask_the_model_to_read_carefully() {
    let text = plain(&input(), &answers(&[("Which library?", "moment please")]));
    assert!(text.starts_with("The user answered: \"Which library?\"=\"moment please\"."));
    assert_eq!(plain(&input(), &BTreeMap::new()), texts::NO_ANSWER);
    // 多选的连接文本必须能按 `join` 的写法还原，否则算作自由文本。
    let loose = plain(
        &input(),
        &answers(&[("Which features?", "Auth, Logs, metrics")]),
    );
    assert!(loose.starts_with("The user answered:"), "{loose}");
}

#[tokio::test]
async fn the_tool_asks_and_maps_the_answer() {
    let answer = QuestionAnswer::Answered {
        answers: answers(&[("Which library?", "date-fns")]),
        annotations: Default::default(),
        response: None,
        follow_up: false,
        afk_timeout_ms: None,
    };
    let interactions = Arc::new(ScriptedInteractions::new([
        serde_json::to_value(answer).unwrap()
    ]));
    let tool = AskUserQuestionTool::new(true);
    let context = Ctx::new(interactions.clone()).build();
    let (progress, _) = mpsc::channel(1);
    let output = tool.call(input(), context, progress).await.unwrap();
    assert!(!output.is_error);
    assert!(output_text(&output).starts_with("Your questions have been answered:"));
    match &interactions.asked()[..] {
        [InteractionRequest::Question { questions, .. }] => {
            assert_eq!(questions, &input()["questions"]);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn declining_rejects_the_call() {
    let interactions = Arc::new(ScriptedInteractions::new([serde_json::to_value(
        QuestionAnswer::Declined,
    )
    .unwrap()]));
    let tool = AskUserQuestionTool::new(true);
    let (progress, _) = mpsc::channel(1);
    let output = tool
        .call(input(), Ctx::new(interactions).build(), progress)
        .await
        .unwrap();
    assert!(output.is_error);
    assert_eq!(output_text(&output), REJECT_MESSAGE);
}

#[tokio::test]
async fn validation_follows_the_schema_bounds() {
    let tool = AskUserQuestionTool::new(true);
    let context = Ctx::new(Arc::new(ScriptedInteractions::default())).build();
    assert_eq!(
        tool.validate_input(&input(), &context).await,
        Validation::Ok
    );
    let single = json!({"questions": [{"question": "Q?", "header": "H", "multiSelect": false,
        "options": [{"label": "only", "description": "d"}]}]});
    let Validation::Invalid { message, .. } = tool.validate_input(&single, &context).await else {
        panic!("a single option must be rejected");
    };
    assert!(message.contains(texts::SINGLE_OPTION_STEER));
    let mut duplicate = input();
    duplicate["questions"][1]["question"] = json!("Which library?");
    let Validation::Invalid { message, .. } = tool.validate_input(&duplicate, &context).await
    else {
        panic!("duplicate questions must be rejected");
    };
    assert!(message.ends_with(texts::QUESTIONS_NOT_UNIQUE));
}

#[tokio::test]
async fn only_interactive_sessions_offer_it_and_dont_ask_denies_it() {
    assert!(AskUserQuestionTool::new(true).is_enabled());
    assert!(!AskUserQuestionTool::new(false).is_enabled());
    assert!(!AskUserQuestionTool::new(true).should_defer());
    let tool = AskUserQuestionTool::new(true);
    let interactions = Arc::new(ScriptedInteractions::default());
    let allowed = tool
        .check_permissions(&input(), &Ctx::new(interactions.clone()).build())
        .await;
    assert_eq!(
        allowed,
        PermissionDecision::Allow {
            updated_input: None
        }
    );
    let mut context = Ctx::new(interactions);
    context.mode = PermissionMode::DontAsk;
    assert!(matches!(
        tool.check_permissions(&input(), &context.build()).await,
        PermissionDecision::Ask { .. }
    ));
}

fn layout() -> Value {
    json!({"questions": [{"question": "Which layout should the page use?", "header": "Layout",
        "multiSelect": false, "options": [
            {"label": "Sidebar", "description": "Navigation on the left", "preview": "```\nnav\n```"},
            {"label": "Top bar", "description": "Navigation on top", "preview": "## Top bar\n\n- one\n- two"}]}]})
}

fn annotation(preview: Option<&str>, notes: Option<&str>) -> BTreeMap<String, QuestionAnnotation> {
    [(
        "Which layout should the page use?".to_owned(),
        QuestionAnnotation {
            preview: preview.map(str::to_owned),
            notes: notes.map(str::to_owned),
        },
    )]
    .into()
}

/// 期望值是对照实现的提问框上实测的结果文字（docs/specs/rust-cli/tools-more.md §1.1）。
#[test]
fn previews_and_notes_follow_the_answer() {
    let chosen = answers(&[("Which layout should the page use?", "Top bar")]);
    let noted = annotation(Some("## Top bar\n\n- one\n- two"), Some("wider"));
    let given = |answers, annotations, follow_up| Answers {
        answers,
        annotations,
        response: None,
        follow_up,
        afk_timeout_ms: None,
    };
    assert_eq!(
        result_text(&layout(), &given(&chosen, &noted, false)),
        "The user answered: \"Which layout should the page use?\"=\"Top bar\" selected preview:\n## Top bar\n\n- one\n- two notes: wider. Read the answers carefully \u{2014} they may request clarification, changes, or that you not proceed \u{2014} and follow what they actually say."
    );
    // 只有备注、没有选：`(notes only)` 写成没有选。
    let notes_only = answers(&[("Which layout should the page use?", "(notes only)")]);
    let only_notes = annotation(None, Some("wider"));
    assert_eq!(
        result_text(&layout(), &given(&notes_only, &only_notes, false)),
        "The user answered: \"Which layout should the page use?\"=(no option selected) notes: wider. Read the answers carefully \u{2014} they may request clarification, changes, or that you not proceed \u{2014} and follow what they actually say."
    );
    // 只有预览、没有备注：答案在选项里。
    let previewed = annotation(Some("## Top bar"), None);
    assert_eq!(
        result_text(&layout(), &given(&chosen, &previewed, false)),
        "Your questions have been answered: \"Which layout should the page use?\"=\"Top bar\" selected preview:\n## Top bar. You can now continue with these answers in mind."
    );
}

#[test]
fn idle_timeouts_say_how_long_and_what_was_selected() {
    let none = BTreeMap::new();
    let afk = |answers, ms| Answers {
        answers,
        annotations: &none,
        response: Some("ignored"),
        follow_up: true,
        afk_timeout_ms: Some(ms),
    };
    let empty = BTreeMap::new();
    assert_eq!(
        result_text(&layout(), &afk(&empty, 4000)),
        "No response after 4s \u{2014} the user may be away from keyboard. Proceed using your best judgment based on the context so far; you can re-ask this question later if it's still relevant."
    );
    let chosen = answers(&[("Which layout should the page use?", "Sidebar")]);
    assert_eq!(
        result_text(&layout(), &afk(&chosen, 60_499)),
        "No response after 60s \u{2014} the user may be away from keyboard. Proceed using your best judgment based on the context so far; you can re-ask this question later if it's still relevant.\n\nBefore going idle the user had selected: \"Which layout should the page use?\"=\"Sidebar\"."
    );
}

#[test]
fn follow_ups_and_responses() {
    let none = BTreeMap::new();
    let chosen = answers(&[("Which layout should the page use?", "Sidebar")]);
    let given = |answers, response, follow_up| Answers {
        answers,
        annotations: &none,
        response,
        follow_up,
        afk_timeout_ms: None,
    };
    assert_eq!(
        result_text(&layout(), &given(&chosen, Some("tabs too"), true)),
        "The user asked for more questions before you proceed. So far they answered: \"Which layout should the page use?\"=\"Sidebar\". They also wrote: \"tabs too\". Call AskUserQuestion again with follow-up questions that build on these answers (do not repeat these); do not start the task yet."
    );
    let empty = BTreeMap::new();
    assert_eq!(
        result_text(&layout(), &given(&empty, Some("  "), true)),
        "The user asked for more questions before answering. Call AskUserQuestion again now with further questions about this decision (do not repeat these); do not proceed with the task yet."
    );
    assert_eq!(
        result_text(&layout(), &given(&chosen, Some("use tabs"), false)),
        "The user responded: use tabs"
    );
}
