use std::sync::Arc;
use std::time::Duration;

use futures_util::stream::{self, BoxStream, StreamExt};
use rust_cli_base::TaskScope;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{
    CommandId, EnvironmentId, Generation, InteractionId, SessionId, TraceId,
};
use rust_cli_kernel_api::model::{ModelError, ModelEvent, ModelPort, ModelRequest};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{Command, Delivery, KernelEvent, StartSource, TurnOutcome};
use rust_cli_kernel_api::store::{Fact, SessionKind, SessionMeta, SessionStore};
use rust_cli_kernel_api::testing::{
    FailingExecutor, FixedClock, FixedContext, MemoryArtifacts, MemoryStore, NeverCompact, NoHooks,
    NoTools, ScriptedModel, SequentialIds, UnusedEnvironment, selection, text_response,
};
use rust_cli_query::{QueryLoop, QueryParts};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use super::*;

fn meta() -> SessionMeta {
    SessionMeta {
        id: SessionId::new("sess_1"),
        kind: SessionKind::Interactive,
        parent: None,
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/work"),
        },
        title: String::new(),
        trace: TraceId::new("trace_1"),
        created_at_ms: 0,
    }
}

/// 没有工具、执行环境不可用的会话（test_support 的组装）。
async fn fixture(model: Arc<dyn ModelPort>, create: bool) -> crate::test_support::Built {
    let mut setup = crate::test_support::Setup::new(model);
    setup.env = Arc::new(UnusedEnvironment);
    setup.stored = create;
    setup.build().await
}

fn submit(id: &str, text: &str) -> CommandEnvelope {
    CommandEnvelope {
        id: CommandId::new(id),
        command: Command::Submit {
            text: text.to_owned(),
            attachments: Vec::new(),
            delivery: Delivery::Queue,
        },
    }
}

/// 读事件直到第 `count` 个 TurnFinished。
async fn until_finished(
    events: &mut BoxStream<'static, SequencedEvent>,
    count: usize,
) -> Vec<KernelEvent> {
    let mut seen = Vec::new();
    let mut finished = 0;
    while finished < count {
        let event = tokio::time::timeout(Duration::from_secs(5), events.next())
            .await
            .unwrap()
            .unwrap();
        if matches!(event.event, KernelEvent::TurnFinished { .. }) {
            finished += 1;
        }
        seen.push(event.event);
    }
    seen
}

#[tokio::test]
async fn a_submitted_text_runs_a_turn_and_commits_both_messages() {
    let fixture = fixture(
        Arc::new(ScriptedModel::new(vec![text_response(&["hi", "!"])])),
        true,
    )
    .await;
    let mut events = fixture.session.subscribe(Some(0));
    assert!(matches!(
        fixture.session.send(submit("c1", "hello")).await,
        Ack::Accepted { .. }
    ));
    let seen = until_finished(&mut events, 1).await;
    assert!(matches!(seen[0], KernelEvent::SessionCreated { .. }));
    assert!(matches!(seen[1], KernelEvent::TurnStarted { .. }));
    let deltas = seen
        .iter()
        .filter(|event| matches!(event, KernelEvent::StreamDelta { .. }))
        .count();
    assert_eq!(deltas, 2);
    assert!(matches!(
        seen.last(),
        Some(KernelEvent::TurnFinished {
            outcome: TurnOutcome::Success,
            ..
        })
    ));
    let facts = fixture.store.facts(&SessionId::new("sess_1"));
    // 两条消息，加上这次请求与回合的计量（usage.md §2）。
    assert_eq!(facts.len(), 4);
    assert!(matches!(facts[0], Fact::MessageAppended { .. }));
    assert!(matches!(facts[1], Fact::MessageAppended { .. }));
    assert!(matches!(facts[2], Fact::ModelCalled { .. }));
    assert!(matches!(facts[3], Fact::TurnEnded { .. }));
    assert_eq!(fixture.session.view().await.unwrap().messages.len(), 2);
    assert_eq!(
        fixture.session.send(submit("c1", "again")).await,
        Ack::Duplicate
    );
}

/// 等到被放行或取消才结束的模型。
struct GatedModel {
    gate: Arc<Notify>,
}

impl ModelPort for GatedModel {
    fn stream(
        &self,
        _request: ModelRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<ModelEvent, ModelError>> {
        let gate = self.gate.clone();
        stream::once(async move {
            tokio::select! {
                () = gate.notified() => None,
                () = cancel.cancelled() => Some(Err(ModelError::Cancelled)),
            }
        })
        .filter_map(|item| async move { item })
        .chain(stream::iter(text_response(&["ok"])))
        .boxed()
    }
}

#[tokio::test]
async fn input_while_running_is_queued_and_interrupt_cancels_the_turn() {
    let gate = Arc::new(Notify::new());
    let fixture = fixture(Arc::new(GatedModel { gate: gate.clone() }), true).await;
    let mut events = fixture.session.subscribe(Some(0));
    fixture.session.send(submit("c1", "first")).await;
    fixture.session.send(submit("c2", "second")).await;
    assert_eq!(fixture.session.view().await.unwrap().queue.len(), 1);
    let interrupt = CommandEnvelope {
        id: CommandId::new("c3"),
        command: Command::Interrupt,
    };
    assert!(matches!(
        fixture.session.send(interrupt).await,
        Ack::Accepted { .. }
    ));
    let first = until_finished(&mut events, 1).await;
    assert!(matches!(
        first.last(),
        Some(KernelEvent::TurnFinished {
            outcome: TurnOutcome::Interrupted,
            ..
        })
    ));
    // 被中断后，排队的输入作为下一回合开始。
    gate.notify_one();
    let second = until_finished(&mut events, 1).await;
    assert!(matches!(
        second.last(),
        Some(KernelEvent::TurnFinished {
            outcome: TurnOutcome::Success,
            ..
        })
    ));
    assert!(fixture.session.view().await.unwrap().queue.is_empty());
}

/// 向用户提问、把回答原样作为结果返回的工具。
pub(super) struct AskingTool;

#[async_trait::async_trait]
impl rust_cli_kernel_api::tool::Tool for AskingTool {
    fn name(&self) -> &str {
        "AskUserQuestion"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    fn description(&self, _context: &rust_cli_kernel_api::tool::PromptContext) -> String {
        String::new()
    }

    async fn call(
        &self,
        input: serde_json::Value,
        context: rust_cli_kernel_api::tool::ToolUseContext,
        _progress: tokio::sync::mpsc::Sender<rust_cli_kernel_api::tool::ToolProgress>,
    ) -> Result<rust_cli_kernel_api::tool::ToolOutput, rust_cli_kernel_api::tool::ToolError> {
        let answer = context
            .interactions
            .ask(
                &context.session,
                rust_cli_kernel_api::orchestration::InteractionRequest::Question {
                    tool_use: context.tool_use.clone(),
                    questions: input,
                },
            )
            .await
            .map_err(|error| rust_cli_kernel_api::tool::ToolError::Failed {
                message: error.to_string(),
            })?;
        Ok(rust_cli_kernel_api::tool::ToolOutput {
            content: vec![rust_cli_kernel_api::message::ToolResultContent::Text {
                text: answer.answer.to_string(),
            }],
            is_error: false,
            display: serde_json::Value::Null,
        })
    }
}

/// 第一次响应调用 [`AskingTool`]、第二次回复文本的会话。
async fn asking_session(scope: &TaskScope) -> (Arc<MemoryStore>, Arc<ScriptedModel>, Session) {
    asking_session_with(scope, Arc::new(NoHooks)).await
}

/// 同 [`asking_session`]，会话侧用给定的 hooks（插话的 UserPromptSubmit，steer_tests.rs）。
async fn asking_session_with(
    scope: &TaskScope,
    hooks: Arc<dyn rust_cli_kernel_api::hooks::HookRunner>,
) -> (Arc<MemoryStore>, Arc<ScriptedModel>, Session) {
    let store = Arc::new(MemoryStore::default());
    store.create(meta()).await.unwrap();
    let ids = Arc::new(SequentialIds::default());
    let model = Arc::new(ScriptedModel::new(vec![
        rust_cli_kernel_api::testing::tool_use_response(
            "toolu_1",
            "AskUserQuestion",
            &serde_json::json!({"question": "which?"}),
        ),
        text_response(&["thanks"]),
    ]));
    let runner = Arc::new(QueryLoop::new(QueryParts {
        model: model.clone(),
        context: Arc::new(FixedContext(Vec::new())),
        tools: Arc::new(rust_cli_kernel_api::testing::ListTools(vec![Arc::new(
            AskingTool,
        )])),
        executor: Arc::new(rust_cli_kernel_api::testing::DirectExecutor),
        compactor: Arc::new(NeverCompact),
        hooks: Arc::new(NoHooks),
        ids: ids.clone(),
        clock: Arc::new(rust_cli_kernel_api::testing::FixedClock(0)),
        env: Arc::new(UnusedEnvironment),
        artifacts: store.clone(),
        generation: Generation::new("g1"),
        non_interactive: false,
        limits: rust_cli_query::RequestLimits::default(),
    }));
    let session = Session::spawn(
        SessionDeps {
            agents: Arc::new(rust_cli_kernel_api::agents::NoAgentSignals),
            cron: Arc::new(rust_cli_kernel_api::cron::NoCron),
            store: store.clone(),
            ids,
            runner,
            compactor: Arc::new(NeverCompact),
            env: Arc::new(UnusedEnvironment),
            max_turns: None,
            hooks,
            commands: Arc::new(rust_cli_kernel_api::user_command::NoUserCommands),
            inputs: Arc::new(rust_cli_kernel_api::user_input::NoUserInputs),
            artifacts: Arc::new(MemoryArtifacts::default()),
            clock: Arc::new(FixedClock(0)),
            generation: Generation::new("g1"),
            spawner: scope.spawner(),
            closing: scope.spawner(),
            titles: None,
            side_model: Arc::new(rust_cli_kernel_api::testing::ScriptedModel::new(Vec::new())),
            released: Arc::new(crate::NothingToRelease),
            directory_rules: Arc::new(rust_cli_kernel_api::orchestration::NoDirectoryRules),
            peers: crate::PeerDeps::off(),
        },
        SessionState {
            meta: meta(),
            messages: Vec::new(),
            selection: selection(),
            mode: PermissionMode::Default.into(),
            checkpoints: Vec::new(),
            start: StartSource::Startup,
            turn: None,
        },
        Opened::Created,
    );
    (store, model, session)
}

/// 等到工具向用户提问。
pub(super) async fn asked(events: &mut BoxStream<'static, SequencedEvent>) -> InteractionId {
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), events.next())
            .await
            .unwrap()
            .unwrap();
        if let KernelEvent::InteractionAsked { interaction, .. } = event.event {
            return interaction;
        }
    }
}

pub(super) fn answer(id: &str, interaction: InteractionId) -> CommandEnvelope {
    CommandEnvelope {
        id: CommandId::new(id),
        command: Command::ResolveInteraction {
            interaction,
            answer: serde_json::json!("blue"),
        },
    }
}

#[tokio::test]
async fn a_tool_question_is_persisted_answered_and_resumes_the_tool() {
    let scope = TaskScope::root("test", |_| {});
    let (store, model, session) = asking_session(&scope).await;
    let mut events = session.subscribe(Some(0));
    session.send(submit("c1", "ask me")).await;
    let interaction = asked(&mut events).await;
    assert_eq!(session.view().await.unwrap().pending.len(), 1);
    let ack = session.send(answer("c2", interaction)).await;
    assert!(matches!(ack, Ack::Accepted { .. }));
    let seen = until_finished(&mut events, 1).await;
    assert!(matches!(
        seen.last(),
        Some(KernelEvent::TurnFinished {
            outcome: TurnOutcome::Success,
            ..
        })
    ));
    // 第二次请求里，工具结果是用户的回答。
    let second = &model.requests()[1];
    assert!(format!("{:?}", second.messages).contains("\\\"blue\\\""));
    let facts = store.facts(&SessionId::new("sess_1"));
    let kinds: Vec<&str> = facts
        .iter()
        .filter_map(|fact| match fact {
            Fact::OperationIntent { .. } => Some("intent"),
            Fact::Entry { kind, .. } => Some(kind.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        kinds,
        vec!["intent", "pending_interaction", "interaction_resolved"]
    );
}

#[path = "steer_tests.rs"]
mod steer;

#[path = "notice_tests.rs"]
mod notice;

#[path = "agent_signals_tests.rs"]
mod agent_signals;

#[path = "lane_tests.rs"]
mod lane;

#[path = "close_tests.rs"]
mod close;
