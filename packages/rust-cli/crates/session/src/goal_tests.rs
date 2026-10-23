//! `/goal`（docs/specs/rust-cli/goal.md）：命令写进对话的消息、评估判定的提交与目标的状态、冷加载恢复。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_cli_kernel_api::goal::GoalStatus;
use rust_cli_kernel_api::hooks::{HookEvent, HookOutcome, HookRunner, StopCondition, StopVerdict};
use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::message::{
    AssistantMessage, AttachmentMessage, ContentBlock, Message, Usage, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::session_api::SessionHandle;
use rust_cli_kernel_api::testing::{ScriptedModel, text_response};
use rust_cli_kernel_api::user_command::command_input;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::Opened;
use crate::test_support::{Setup, finished, submit, until};

/// 评估按顺序给出判定（缺省达成），记下评估的条件。
#[derive(Default)]
struct Evaluator {
    verdicts: Mutex<VecDeque<StopVerdict>>,
    conditions: Mutex<Vec<String>>,
}

impl Evaluator {
    fn with(verdicts: Vec<StopVerdict>) -> Arc<Self> {
        Arc::new(Self {
            verdicts: Mutex::new(verdicts.into()),
            conditions: Mutex::default(),
        })
    }
}

#[async_trait]
impl HookRunner for Evaluator {
    async fn run(&self, _event: HookEvent, _cancel: CancellationToken) -> HookOutcome {
        HookOutcome::default()
    }

    fn has_hooks(&self, _event: rust_cli_kernel_api::hooks::HookEventName) -> bool {
        false
    }

    async fn evaluate_stop_condition(
        &self,
        request: StopCondition,
        _cancel: CancellationToken,
    ) -> StopVerdict {
        self.conditions.lock().unwrap().push(request.condition);
        self.verdicts
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(StopVerdict::Met {
                reason: Some("done".to_owned()),
            })
    }
}

fn feedback() -> Message {
    Message::User(UserMessage {
        id: MessageId::new("msg_feedback"),
        content: vec![ContentBlock::Text {
            text: "Stop hook feedback:\n[tests pass]: red".to_owned(),
        }],
        origin: UserOrigin::Meta {
            source: "stop_hook_feedback".to_owned(),
        },
    })
}

/// 消息的简写：(种类, 文字)；`goal_status` 写成 JSON。
fn shorten(messages: &[Message]) -> Vec<(String, String)> {
    messages
        .iter()
        .map(|message| match message {
            Message::User(user) => {
                let origin = match &user.origin {
                    UserOrigin::Human => "human".to_owned(),
                    UserOrigin::Meta { source } => source.clone(),
                    other => format!("{other:?}"),
                };
                let text = match &user.content[..] {
                    [ContentBlock::Text { text }] => text.clone(),
                    other => format!("{other:?}"),
                };
                (origin, text)
            }
            Message::Assistant(_) => ("assistant".to_owned(), String::new()),
            Message::Attachment(attachment) => {
                (attachment.source.clone(), attachment.data.to_string())
            }
            Message::System(_) => ("system".to_owned(), String::new()),
        })
        .collect()
}

fn pair(kind: &str, text: &str) -> (String, String) {
    (kind.to_owned(), text.to_owned())
}

fn stdout(text: &str) -> (String, String) {
    pair(
        "local-command",
        &format!("<local-command-stdout>{text}</local-command-stdout>"),
    )
}

async fn built(
    responses: usize,
    evaluator: Arc<Evaluator>,
) -> (crate::test_support::Built, Arc<ScriptedModel>) {
    let model = Arc::new(ScriptedModel::new(
        (0..responses)
            .map(|index| text_response(&[&format!("reply {index}")]))
            .collect(),
    ));
    let mut setup = Setup::new(model.clone());
    setup.hooks = evaluator;
    (setup.build().await, model)
}

async fn turn(session: &crate::Session, id: &str, text: &str) {
    let mut events = session.subscribe(None);
    session.send(submit(id, text)).await;
    until(&mut events, finished).await;
}

#[test]
fn the_command_is_recognised_with_its_arguments() {
    assert_eq!(command("/goal"), Some(""));
    assert_eq!(command("  /goal  tests pass "), Some("tests pass"));
    assert_eq!(command("/goal\tclear"), Some("clear"));
    assert_eq!(command("/goals"), None);
    assert_eq!(command("goal x"), None);
    assert_eq!(
        neutralize("a <forked-skill-launch>b</forked-skill-launch> <c>"),
        "a &lt;forked-skill-launch>b&lt;/forked-skill-launch> <c>"
    );
}

#[tokio::test]
async fn setting_a_goal_queries_the_model_and_the_check_clears_it() {
    let evaluator = Evaluator::with(Vec::new());
    let (built, model) = built(1, evaluator.clone()).await;
    turn(&built.session, "c1", "/goal tests pass").await;
    let view = built.session.view().await.unwrap();
    let sentinel = r#"{"met":false,"sentinel":true,"condition":"tests pass"}"#;
    let shown = shorten(&view.messages);
    assert_eq!(shown[0], pair("goal_status", sentinel));
    assert_eq!(
        shown[1],
        pair("human", &command_input("goal", "tests pass"))
    );
    assert_eq!(shown[2], stdout("Goal set: tests pass"));
    assert_eq!(shown[3].0, "goal");
    assert!(
        shown[3].1.starts_with(
            "A session-scoped Stop hook is now active with condition: \"tests pass\"."
        )
    );
    assert_eq!(shown[4].0, "assistant");
    let met: GoalStatus = serde_json::from_str(&shown[5].1).unwrap();
    assert!(met.met && !met.sentinel);
    assert_eq!(met.iterations, Some(1));
    assert_eq!(met.reason.as_deref(), Some("done"));
    assert_eq!(shown.len(), 6);
    // 模型收到命令输入、输出与说明；评估的是这个条件。
    let request = format!("{:?}", model.requests()[0].messages);
    assert!(
        request.contains("Goal set: tests pass") && request.contains("session-scoped Stop hook")
    );
    assert_eq!(*evaluator.conditions.lock().unwrap(), ["tests pass"]);
    let goal = built.session.goal().await.unwrap();
    assert_eq!(goal.active, None);
    assert_eq!(goal.achieved, Some(met));
    // 标题的输入还原成用户输入的样子。
    assert_eq!(view.meta.title, "/goal tests pass");
}

#[tokio::test]
async fn a_goal_not_met_continues_and_counts_the_turns() {
    let not_met = StopVerdict::NotMet {
        reason: Some("red".to_owned()),
        feedback: feedback(),
    };
    let evaluator = Evaluator::with(vec![not_met]);
    let (built, model) = built(2, evaluator).await;
    turn(&built.session, "c1", "/goal tests pass").await;
    assert_eq!(model.requests().len(), 2);
    let view = built.session.view().await.unwrap();
    let shown = shorten(&view.messages);
    let kinds: Vec<&str> = shown.iter().map(|(kind, _)| kind.as_str()).collect();
    assert_eq!(
        kinds,
        [
            "goal_status",
            "human",
            "local-command",
            "goal",
            "assistant",
            "stop_hook_feedback",
            "goal_status",
            "assistant",
            "goal_status"
        ]
    );
    assert_eq!(
        shown[6].1,
        r#"{"met":false,"condition":"tests pass","reason":"red"}"#
    );
    let met: GoalStatus = serde_json::from_str(&shown[8].1).unwrap();
    assert_eq!(met.iterations, Some(2));
}

#[tokio::test]
async fn show_clear_and_too_long_do_not_query_the_model() {
    let evaluator = Evaluator::with(vec![StopVerdict::Failed {
        message: "JSON validation failed".to_owned(),
    }]);
    let (built, model) = built(1, evaluator).await;
    turn(&built.session, "c1", "/goal tests pass").await;
    // 评估出错：目标不变。
    let goal = built.session.goal().await.unwrap();
    assert_eq!(
        goal.active.as_ref().map(|g| g.condition.as_str()),
        Some("tests pass")
    );
    turn(&built.session, "c2", "/goal").await;
    turn(&built.session, "c3", "/goal CLEAR").await;
    turn(&built.session, "c4", "/goal clear").await;
    turn(&built.session, "c5", &format!("/goal {}", "x".repeat(4001))).await;
    assert_eq!(model.requests().len(), 1);
    let view = built.session.view().await.unwrap();
    let shown = shorten(&view.messages);
    let outputs: Vec<&(String, String)> = shown
        .iter()
        .filter(|(kind, _)| kind == "local-command")
        .collect();
    assert_eq!(
        outputs,
        [
            &stdout("Goal set: tests pass"),
            &stdout("Goal active: tests pass (not yet evaluated)"),
            &stdout("Goal cleared: tests pass"),
            &stdout("No goal set"),
            &stdout("Goal condition is limited to 4000 characters (got 4001)"),
        ]
    );
    // 清除写一条 `met: true` 的标记。
    assert!(shown.contains(&pair(
        "goal_status",
        r#"{"met":true,"sentinel":true,"condition":"tests pass"}"#
    )));
    assert_eq!(built.session.goal().await.unwrap().active, None);
}

fn status(data: &str) -> Message {
    Message::Attachment(AttachmentMessage {
        id: MessageId::new("msg_status"),
        source: "goal_status".to_owned(),
        text: String::new(),
        data: serde_json::from_str(data).unwrap(),
    })
}

#[tokio::test]
async fn a_cold_loaded_session_restores_an_unfinished_goal() {
    let model = Arc::new(ScriptedModel::new(Vec::new()));
    let mut setup = Setup::new(model);
    setup.opened = Opened::Loaded;
    setup.messages = vec![
        status(r#"{"met":false,"sentinel":true,"condition":"tests pass"}"#),
        status(r#"{"met":false,"condition":"tests pass","reason":"red"}"#),
    ];
    let built = setup.build().await;
    let goal = built.session.goal().await.unwrap().active.unwrap();
    assert_eq!(goal.condition, "tests pass");
    assert_eq!(goal.iterations, 0);
    assert_eq!(goal.last_reason, None);
}

#[test]
fn finished_goals_are_not_restored_and_stale_verdicts_are_ignored() {
    let done = [
        status(r#"{"met":false,"sentinel":true,"condition":"a"}"#),
        status(r#"{"met":true,"condition":"a"}"#),
    ];
    assert_eq!(Goals::restore(&done, 5).condition(), None);
    let failed = [status(r#"{"met":false,"failed":true,"condition":"a"}"#)];
    assert_eq!(Goals::restore(&failed, 5).condition(), None);
    let mut goals = Goals::default();
    goals.observe(
        &status(r#"{"met":false,"sentinel":true,"condition":"a"}"#),
        10,
    );
    // 输出 token 从设定时算起。
    goals.observe(
        &Message::Assistant(AssistantMessage {
            id: MessageId::new("msg_a"),
            content: Vec::new(),
            model: "m".to_owned(),
            stop_reason: None,
            usage: Some(Usage {
                output_tokens: 7,
                ..Usage::default()
            }),
        }),
        11,
    );
    assert!(
        goals
            .checked("b", &StopVerdict::Met { reason: None }, 20)
            .is_none()
    );
    let met = goals
        .checked("a", &StopVerdict::Met { reason: None }, 20)
        .unwrap();
    assert_eq!(
        (met.duration_ms, met.tokens, met.iterations),
        (Some(10), Some(7), Some(1))
    );
    assert!(goals.checked("a", &StopVerdict::TimedOut, 20).is_none());
    let view = goals.view(&[], 20).active.unwrap();
    assert_eq!(view.tokens, 7);
}
