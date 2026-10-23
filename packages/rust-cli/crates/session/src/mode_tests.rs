//! 工具经 `set_mode` 修改模式：先落库再通知，同一回合里后续的工具调用看到新模式
//! （docs/specs/rust-cli/tools-more.md §2.3）。

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use rust_cli_base::TaskScope;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{CommandId, EnvironmentId, Generation, SessionId, TraceId};
use rust_cli_kernel_api::message::ToolResultContent;
use rust_cli_kernel_api::permission::{ExecutionState, PermissionMode};
use rust_cli_kernel_api::session_api::{
    Command, CommandEnvelope, Delivery, KernelEvent, SessionHandle, StartSource,
};
use rust_cli_kernel_api::store::{Fact, SessionKind, SessionMeta, SessionStore};
use rust_cli_kernel_api::testing::{
    DirectExecutor, FixedClock, FixedContext, ListTools, MemoryArtifacts, MemoryStore,
    NeverCompact, NoHooks, ScriptedModel, SequentialIds, UnusedEnvironment, selection,
    text_response, tool_use_response,
};
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext,
};
use rust_cli_query::{QueryLoop, QueryParts};
use serde_json::{Value, json};

use crate::{Opened, Session, SessionDeps, SessionState};

/// 输入里有 `mode` 时请求改成 plan；结果总是调用时看到的模式。
struct ModeTool;

#[async_trait::async_trait]
impl Tool for ModeTool {
    fn name(&self) -> &str {
        "Mode"
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }

    fn description(&self, _context: &PromptContext) -> String {
        String::new()
    }

    async fn call(
        &self,
        input: Value,
        context: ToolUseContext,
        _progress: tokio::sync::mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let seen = format!("{:?}", context.mode);
        if input.get("plan").is_some() {
            context
                .interactions
                .set_mode(&context.session, PermissionMode::Plan)
                .await
                .map_err(|error| ToolError::Failed {
                    message: error.to_string(),
                })?;
        }
        Ok(ToolOutput {
            content: vec![ToolResultContent::Text { text: seen }],
            is_error: false,
            display: Value::Null,
        })
    }
}

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

/// 会话 actor（工具清单里只有 `ModeTool`）。
fn spawn(
    scope: &TaskScope,
    store: &Arc<MemoryStore>,
    model: &Arc<ScriptedModel>,
    mode: PermissionMode,
) -> Session {
    let ids = Arc::new(SequentialIds::default());
    let runner = Arc::new(QueryLoop::new(QueryParts {
        model: model.clone(),
        context: Arc::new(FixedContext(Vec::new())),
        tools: Arc::new(ListTools(vec![Arc::new(ModeTool)])),
        executor: Arc::new(DirectExecutor),
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
    Session::spawn(
        SessionDeps {
            agents: Arc::new(rust_cli_kernel_api::agents::NoAgentSignals),
            cron: Arc::new(rust_cli_kernel_api::cron::NoCron),
            store: store.clone(),
            ids,
            runner,
            compactor: Arc::new(NeverCompact),
            env: Arc::new(UnusedEnvironment),
            max_turns: None,
            hooks: Arc::new(NoHooks),
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
            mode: mode.into(),
            checkpoints: Vec::new(),
            start: StartSource::Startup,
            turn: None,
        },
        Opened::Created,
    )
}

#[tokio::test]
async fn a_tool_changes_the_mode_for_the_rest_of_the_turn() {
    let scope = TaskScope::root("test", |_| {});
    let store = Arc::new(MemoryStore::default());
    store.create(meta()).await.unwrap();
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "Mode", &json!({"plan": true})),
        tool_use_response("toolu_2", "Mode", &json!({})),
        text_response(&["done"]),
    ]));
    let session = spawn(&scope, &store, &model, PermissionMode::Default);
    let mut events = session.subscribe(Some(0));
    session
        .send(CommandEnvelope {
            id: CommandId::new("c1"),
            command: Command::Submit {
                text: "plan it".to_owned(),
                attachments: Vec::new(),
                delivery: Delivery::Queue,
            },
        })
        .await;
    let mut seen = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), events.next())
            .await
            .unwrap()
            .unwrap()
            .event;
        let finished = matches!(event, KernelEvent::TurnFinished { .. });
        seen.push(event);
        if finished {
            break;
        }
    }
    // 修复线 F20：`set_mode(plan)` 是旧写法，只打开 plan；权限模式不变，事件是 PlanChanged。
    assert!(
        seen.iter()
            .any(|event| matches!(event, KernelEvent::PlanChanged { enabled: true }))
    );
    assert!(
        !seen
            .iter()
            .any(|event| matches!(event, KernelEvent::ModeChanged { .. }))
    );
    let view = session.view().await.unwrap();
    assert_eq!((view.mode, view.plan), (PermissionMode::Default, true));
    // 第一个调用看到的是旧模式，第二个调用（同一回合）看到 plan。
    let requests = model.requests();
    let third = format!("{:?}", requests[2].messages);
    assert!(third.contains("Default"), "{third}");
    assert!(third.contains("Plan"), "{third}");
    // 模式先落库（没有命令 ID：来自工具）。
    let facts = store.facts(&SessionId::new("sess_1"));
    assert!(facts.contains(&Fact::ExecutionChanged {
        state: ExecutionState::from_parts(PermissionMode::Default, true),
    }));
    // 工具切换不写本机项目的持久模式（Node 只在 `setMode` 写，config.md §2.2）。
    assert!(
        !facts
            .iter()
            .any(|fact| matches!(fact, Fact::ProjectModeSet { .. }))
    );
}

/// 用户切换模式：与 `ExecutionChanged` 同一批写项目持久模式，值是切换后的权限模式（Node
/// `saveProjectPermissionMode({mode: runtime.getMode()})`，config.md §2.2）。修复线 F20：`plan` 只打开 plan、写当前的
/// 权限模式（每次都写，同 Node）；改权限模式不动 plan；`SetPlan` 不写持久模式。
#[tokio::test]
async fn user_mode_switches_write_the_project_mode() {
    let scope = TaskScope::root("test", |_| {});
    let store = Arc::new(MemoryStore::default());
    store.create(meta()).await.unwrap();
    let model = Arc::new(ScriptedModel::new(Vec::new()));
    let session = spawn(&scope, &store, &model, PermissionMode::Default);
    let switches = [
        PermissionMode::AcceptEdits,
        PermissionMode::Plan,
        PermissionMode::Plan,
        PermissionMode::DontAsk,
    ];
    for (index, mode) in switches.into_iter().enumerate() {
        let ack = session
            .send(CommandEnvelope {
                id: CommandId::new(format!("mode_{index}")),
                command: Command::SetMode { mode },
            })
            .await;
        assert!(
            matches!(ack, rust_cli_kernel_api::session_api::Ack::Accepted { .. }),
            "{ack:?}"
        );
    }
    let ack = session
        .send(CommandEnvelope {
            id: CommandId::new("plan_off"),
            command: Command::SetPlan { enabled: false },
        })
        .await;
    assert!(matches!(
        ack,
        rust_cli_kernel_api::session_api::Ack::Accepted { .. }
    ));
    let facts: Vec<Fact> = store
        .facts(&SessionId::new("sess_1"))
        .into_iter()
        .filter(|fact| {
            matches!(
                fact,
                Fact::ExecutionChanged { .. } | Fact::ProjectModeSet { .. }
            )
        })
        .collect();
    let changed = |mode, plan| Fact::ExecutionChanged {
        state: ExecutionState::from_parts(mode, plan),
    };
    let saved = |mode| Fact::ProjectModeSet { mode };
    assert_eq!(
        facts,
        [
            changed(PermissionMode::AcceptEdits, false),
            saved(PermissionMode::AcceptEdits),
            changed(PermissionMode::AcceptEdits, true),
            saved(PermissionMode::AcceptEdits),
            changed(PermissionMode::AcceptEdits, true),
            saved(PermissionMode::AcceptEdits),
            changed(PermissionMode::DontAsk, true),
            saved(PermissionMode::DontAsk),
            changed(PermissionMode::DontAsk, false),
        ]
    );
    let view = session.view().await.unwrap();
    assert_eq!((view.mode, view.plan), (PermissionMode::DontAsk, false));
}
