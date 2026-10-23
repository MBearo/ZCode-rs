//! `/goal` 的停止前检查（docs/specs/rust-cli/goal.md §4）：有目标时评估、未达成时继续、达成时结束、子代理不评估。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::hooks::{HookEvent, HookOutcome, HookRunner, StopCondition, StopVerdict};
use rust_cli_kernel_api::ids::{AgentId, InteractionId, MessageId, OperationId, SessionId};
use rust_cli_kernel_api::message::{ContentBlock, Message, UserMessage, UserOrigin};
use rust_cli_kernel_api::model::{ApiRole, QuerySource};
use rust_cli_kernel_api::orchestration::{
    InteractionAnswer, InteractionRequest, Interactions, OrchestrationError, QueryEvent, QueryStop,
    RunSink,
};
use rust_cli_kernel_api::store::StoreError;
use rust_cli_kernel_api::testing::{RecordingSink, ScriptedModel, text_response};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::RunScope;
use crate::run::tests::{parts, request};

/// 评估按顺序返回给定的判定，记下收到的条件与对话记录。
#[derive(Default)]
struct Evaluator {
    verdicts: Mutex<VecDeque<StopVerdict>>,
    asked: Mutex<Vec<StopCondition>>,
}

#[async_trait]
impl HookRunner for Evaluator {
    async fn run(&self, _event: HookEvent, _cancel: CancellationToken) -> HookOutcome {
        HookOutcome::default()
    }

    async fn evaluate_stop_condition(
        &self,
        request: StopCondition,
        _cancel: CancellationToken,
    ) -> StopVerdict {
        self.asked.lock().unwrap().push(request);
        self.verdicts
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(StopVerdict::Met { reason: None })
    }
}

/// 带目标的输出端：判定未达成时把反馈当作已提交的消息返回，达成时清掉目标。
struct GoalSink {
    inner: RecordingSink,
    goal: Mutex<Option<String>>,
    reports: Mutex<Vec<(String, StopVerdict)>>,
}

impl GoalSink {
    fn new(goal: Option<&str>) -> Self {
        Self {
            inner: RecordingSink::default(),
            goal: Mutex::new(goal.map(str::to_owned)),
            reports: Mutex::default(),
        }
    }
}

#[async_trait]
impl Interactions for GoalSink {
    async fn ask(
        &self,
        _session: &SessionId,
        _request: InteractionRequest,
    ) -> Result<InteractionAnswer, OrchestrationError> {
        Ok(InteractionAnswer {
            interaction: InteractionId::new("interaction_test"),
            answer: Value::Null,
        })
    }
}

#[async_trait]
impl RunSink for GoalSink {
    async fn commit_message(&self, message: Message) -> Result<(), StoreError> {
        self.inner.commit_message(message).await
    }

    async fn commit_intent(
        &self,
        operation: OperationId,
        fingerprint: String,
    ) -> Result<(), StoreError> {
        self.inner.commit_intent(operation, fingerprint).await
    }

    async fn emit(&self, event: QueryEvent) -> bool {
        self.inner.emit(event).await
    }

    async fn current_goal(&self) -> Option<String> {
        self.goal.lock().unwrap().clone()
    }

    async fn goal_checked(
        &self,
        condition: String,
        verdict: StopVerdict,
    ) -> Result<Vec<Message>, StoreError> {
        self.reports
            .lock()
            .unwrap()
            .push((condition, verdict.clone()));
        Ok(match verdict {
            StopVerdict::NotMet { feedback, .. } => vec![feedback],
            StopVerdict::Met { .. } | StopVerdict::Impossible { .. } => {
                *self.goal.lock().unwrap() = None;
                Vec::new()
            }
            _ => Vec::new(),
        })
    }
}

fn feedback(text: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(format!("msg_feedback_{text}")),
        content: vec![ContentBlock::Text {
            text: format!("Stop hook feedback:\n[tests pass]: {text}"),
        }],
        origin: UserOrigin::Meta {
            source: "stop_hook_feedback".to_owned(),
        },
    })
}

struct Ran {
    stop: QueryStop,
    requests: usize,
    asked: Vec<StopCondition>,
    reports: Vec<(String, StopVerdict)>,
    messages: Vec<Message>,
}

async fn run(goal: Option<&str>, verdicts: Vec<StopVerdict>, agent: Option<AgentId>) -> Ran {
    let model = Arc::new(ScriptedModel::new(
        (0..3)
            .map(|index| text_response(&[&format!("reply {index}")]))
            .collect(),
    ));
    let evaluator = Arc::new(Evaluator {
        verdicts: Mutex::new(verdicts.into()),
        asked: Mutex::default(),
    });
    let mut parts = parts(model.clone(), Vec::new());
    parts.hooks = evaluator.clone();
    let request = request("go");
    let sink = Arc::new(GoalSink::new(goal));
    let scope = Arc::new(RunScope {
        session: request.session.clone(),
        run: request.run.clone(),
        workspace: request.workspace.clone(),
        agent,
        mode: request.mode,
        abort: Abort::new(),
        sink: sink.clone(),
        parts,
        exhausted: Default::default(),
    });
    let stop = crate::run::run(scope, request).await.unwrap();
    let asked = std::mem::take(&mut *evaluator.asked.lock().unwrap());
    let reports = sink.reports.lock().unwrap().clone();
    Ran {
        stop,
        requests: model.requests().len(),
        asked,
        reports,
        messages: sink.inner.messages(),
    }
}

#[tokio::test]
async fn without_a_goal_nothing_is_evaluated() {
    let ran = run(None, Vec::new(), None).await;
    assert_eq!(ran.stop, QueryStop::Completed);
    assert_eq!(ran.requests, 1);
    assert!(ran.asked.is_empty());
}

#[tokio::test]
async fn a_goal_not_met_continues_the_turn_until_it_is_met() {
    let not_met = StopVerdict::NotMet {
        reason: Some("red".to_owned()),
        feedback: feedback("red"),
    };
    let ran = run(
        Some("tests pass"),
        vec![not_met.clone(), StopVerdict::Met { reason: None }],
        None,
    )
    .await;
    assert_eq!(ran.stop, QueryStop::Completed);
    assert_eq!(ran.requests, 2);
    assert_eq!(ran.asked.len(), 2);
    assert_eq!(ran.asked[0].condition, "tests pass");
    // 对话记录是 normalize 后的活动历史：以最后一条回复结尾。
    let first = &ran.asked[0].transcript;
    assert_eq!(first.last().map(|m| m.role), Some(ApiRole::Assistant));
    // 第二次评估看到了第一次的反馈与第二条回复；Stop 事件带 stop_hook_active。
    assert_eq!(ran.asked[1].transcript.len(), first.len() + 2);
    assert_eq!(ran.asked[1].event.payload["stop_hook_active"], true);
    assert_eq!(ran.reports.len(), 2);
    assert_eq!(ran.reports[0], ("tests pass".to_owned(), not_met));
    // 反馈由 actor 提交，主循环不再提交一次。
    assert!(!ran.messages.contains(&feedback("red")));
}

#[tokio::test]
async fn failures_and_impossible_goals_end_the_turn() {
    for verdict in [
        StopVerdict::Impossible { reason: None },
        StopVerdict::Failed {
            message: "JSON validation failed".to_owned(),
        },
        StopVerdict::TimedOut,
    ] {
        let ran = run(Some("tests pass"), vec![verdict], None).await;
        assert_eq!(ran.stop, QueryStop::Completed);
        assert_eq!(ran.requests, 1);
        assert_eq!(ran.reports.len(), 1);
    }
}

#[tokio::test]
async fn subagents_do_not_check_the_goal() {
    let ran = run(
        Some("tests pass"),
        Vec::new(),
        Some(AgentId::new("agent_1")),
    )
    .await;
    assert_eq!(ran.stop, QueryStop::Completed);
    assert!(ran.asked.is_empty());
    let _ = QuerySource::MainThread;
}
