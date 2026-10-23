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

#[test]
fn answers_inside_the_options_read_as_answered() {
    let text = result_text(
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
    let text = result_text(&input(), &answers(&[("Which library?", "moment please")]));
    assert!(text.starts_with("The user answered: \"Which library?\"=\"moment please\"."));
    assert_eq!(result_text(&input(), &BTreeMap::new()), texts::NO_ANSWER);
    // 多选的连接文本必须能按 `join` 的写法还原，否则算作自由文本。
    let loose = result_text(
        &input(),
        &answers(&[("Which features?", "Auth, Logs, metrics")]),
    );
    assert!(loose.starts_with("The user answered:"), "{loose}");
}

#[tokio::test]
async fn the_tool_asks_and_maps_the_answer() {
    let answer = QuestionAnswer::Answered {
        answers: answers(&[("Which library?", "date-fns")]),
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
