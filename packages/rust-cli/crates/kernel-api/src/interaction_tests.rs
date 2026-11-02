//! 交互的回答（工作线 F134）：JSON 的形状同里面那种回答；日志的决定与记住的规则；没有人能回答时按询问的种类。

use serde_json::json;

use super::*;
use crate::ids::ToolUseId;
use crate::permission::{PermissionMode, PermissionRule};

fn question() -> InteractionRequest {
    InteractionRequest::Question {
        tool_use: ToolUseId::new("tu1"),
        questions: json!([]),
    }
}

#[test]
fn an_answer_has_the_json_of_the_answer_inside() {
    let answers = [
        Answer::from(PermissionAnswer::Deny { feedback: None }),
        Answer::from(QuestionAnswer::Declined),
        Answer::from(PlanApprovalAnswer::Approve {
            mode: PermissionMode::Default,
        }),
    ];
    let json = answers
        .iter()
        .map(|answer| serde_json::to_value(answer).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        json,
        [
            json!({"decision": "deny"}),
            json!({"decision": "declined"}),
            json!({"decision": "approve", "mode": "default"}),
        ]
    );
    let back = json
        .into_iter()
        .map(|value| serde_json::from_value::<Answer>(value).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(back, answers);
}

#[test]
fn the_decision_and_the_remembered_rules_come_from_the_answer() {
    let rule = PermissionRule {
        tool: "Bash".to_owned(),
        content: Some("npm test".to_owned()),
    };
    let allow = Answer::from(PermissionAnswer::Allow {
        remember: vec![rule.clone()],
    });
    assert_eq!(
        (allow.decision(), allow.remembered()),
        ("allow", &[rule][..])
    );
    let keep = Answer::from(PlanApprovalAnswer::KeepPlanning { feedback: None });
    assert_eq!(
        (keep.decision(), keep.remembered()),
        ("keep_planning", &[][..])
    );
}

#[test]
fn nobody_answers_each_kind_of_question_in_its_own_way() {
    assert_eq!(
        Answer::nobody(&question(), PermissionAnswer::Unavailable),
        Answer::Question(QuestionAnswer::Declined)
    );
    // 种类不对的回答退回原样（工具按没有回答处理）。
    let declined = Answer::Question(QuestionAnswer::Declined);
    assert_eq!(
        PermissionAnswer::try_from(declined.clone()),
        Err(declined.clone())
    );
    assert_eq!(
        QuestionAnswer::try_from(declined),
        Ok(QuestionAnswer::Declined)
    );
}
