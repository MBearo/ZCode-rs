//! 测试替身：假的子循环工厂（记录它看到的工具、system 段与请求）、调用工具的父运行。

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use rust_cli_base::TaskScope;
use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::agents::{SubagentQueries, TaskListStore};
use rust_cli_kernel_api::env::{EnvPath, Environment, OperationContext, Origin, WorkspaceRef};
use rust_cli_kernel_api::ids::{
    EnvironmentId, Generation, OperationId, OwnerEpoch, RunId, SessionId, ToolUseId,
};
use rust_cli_kernel_api::message::ToolResultContent;
use rust_cli_kernel_api::message::{AssistantMessage, ContentBlock, Message, StopReason, Usage};
use rust_cli_kernel_api::model::{QuerySource, SystemBlock};
use rust_cli_kernel_api::orchestration::{
    ContextBuilder, ContextRequest, QueryError, QueryRequest, QueryRunner, QueryStop, RunSink,
    ToolRegistry,
};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::store::SessionStore;
use rust_cli_kernel_api::testing::{
    FakeTool, FixedClock, FixedContext, ListTools, MemoryStore, RecordingSink, SequentialIds,
    UnusedEnvironment, selection,
};
use rust_cli_kernel_api::tool::{Tool, ToolOutput, ToolUseContext, Validation};
use serde_json::Value;
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;

use crate::{Agents, AgentsConfig, AgentsPorts, MemoryTaskLists};

/// 子循环看到的东西。
#[derive(Clone)]
pub(crate) struct Seen {
    pub(crate) tools: Vec<String>,
    pub(crate) system: Vec<SystemBlock>,
    pub(crate) request: QueryRequest,
}

/// 子循环在哪里等（后台场景）；中止时提前返回。
#[derive(Clone)]
pub(crate) enum Gate {
    /// 所有运行共用一个。
    Shared(Arc<Notify>),
    /// 每个子会话一个（嵌套场景：分别放行），见 [`Gates::release`]。
    PerSession(Gates),
}

/// 按子会话的放行信号；先放行后等待也不会丢（`Notify` 存一个许可）。
#[derive(Clone, Default)]
pub(crate) struct Gates(Arc<Mutex<std::collections::HashMap<SessionId, Arc<Notify>>>>);

impl Gates {
    fn of(&self, session: &SessionId) -> Arc<Notify> {
        let mut gates = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        gates.entry(session.clone()).or_default().clone()
    }

    pub(crate) fn release(&self, session: &str) {
        self.of(&SessionId::new(session)).notify_one();
    }
}

#[derive(Default)]
pub(crate) struct FakeQueries {
    pub(crate) seen: Arc<Mutex<Vec<Seen>>>,
    pub(crate) gate: Option<Gate>,
    /// 放行后、结束前在“工具边界”取走的通知：(子会话, 通知全文)。
    pub(crate) absorbed: Arc<Mutex<Vec<(SessionId, String)>>>,
}

struct FakeRunner {
    tools: Arc<dyn ToolRegistry>,
    context: Arc<dyn ContextBuilder>,
    seen: Arc<Mutex<Vec<Seen>>>,
    gate: Option<Gate>,
    absorbed: Arc<Mutex<Vec<(SessionId, String)>>>,
}

impl SubagentQueries for FakeQueries {
    fn runner(
        &self,
        tools: Arc<dyn ToolRegistry>,
        context: Arc<dyn ContextBuilder>,
    ) -> Arc<dyn QueryRunner> {
        Arc::new(FakeRunner {
            tools,
            context,
            seen: self.seen.clone(),
            gate: self.gate.clone(),
            absorbed: self.absorbed.clone(),
        })
    }
}

pub(crate) fn report_message(text: &str) -> Message {
    Message::Assistant(AssistantMessage {
        id: rust_cli_kernel_api::ids::MessageId::new("msg_report"),
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        model: "claude-opus-5-5".to_owned(),
        stop_reason: Some(StopReason::EndTurn),
        usage: Some(Usage {
            input_tokens: 10,
            output_tokens: 3,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        }),
    })
}

#[async_trait]
impl QueryRunner for FakeRunner {
    async fn run(
        &self,
        request: QueryRequest,
        sink: Arc<dyn RunSink>,
        abort: Abort,
    ) -> Result<QueryStop, QueryError> {
        let context = ContextRequest {
            capabilities: None,
            session: request.session.clone(),
            workspace: request.workspace.clone(),
            agent: request.agent.clone(),
            source: request.source.clone(),
            mode: request.mode,
            tool_names: Vec::new(),
            selection: request.selection.clone(),
            plan_flags: Default::default(),
            mcp: None,
        };
        let system = self
            .context
            .build(context, &request.messages)
            .await
            .unwrap()
            .system;
        let tools = self
            .tools
            .tools()
            .iter()
            .map(|tool| tool.name().to_owned())
            .collect();
        let session = request.session.clone();
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Seen {
                tools,
                system,
                request,
            });
        let gate = match &self.gate {
            Some(Gate::Shared(gate)) => Some(gate.clone()),
            Some(Gate::PerSession(gates)) => Some(gates.of(&session)),
            None => None,
        };
        if let Some(gate) = gate {
            tokio::select! {
                () = gate.notified() => {}
                () = abort.aborted() => return Ok(QueryStop::Aborted),
            }
        }
        // 一个工具边界：取走发给这次运行的通知（query.md §5.1）。
        for notice in sink.take_notices().await {
            self.absorbed
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((session.clone(), notice));
        }
        sink.commit_message(report_message("Hello from the subagent."))
            .await?;
        Ok(QueryStop::Completed)
    }
}

/// 父运行：在登记过的运行里调用一个工具，记下结果。校验不过时结果同执行管线
/// （`<tool_use_error>…</tool_use_error>`，is_error）。
pub(crate) struct ParentRunner {
    pub(crate) registry: Arc<dyn ToolRegistry>,
    /// 工具调用看到的执行环境。
    pub(crate) env: Arc<dyn Environment>,
    pub(crate) tool: String,
    pub(crate) input: Value,
    pub(crate) output: Arc<Mutex<Option<ToolOutput>>>,
}

pub(crate) fn workspace() -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new("/Users/dev/work"),
    }
}

pub(crate) fn tool_context(session: &SessionId, run: &RunId) -> ToolUseContext {
    ToolUseContext {
        session: session.clone(),
        run: run.clone(),
        tool_use: ToolUseId::new("toolu_1"),
        agent: None,
        workspace: workspace(),
        mode: PermissionMode::DontAsk,
        cancel: CancellationToken::new(),
        env: Arc::new(UnusedEnvironment),
        artifacts: Arc::new(MemoryStore::default()),
        interactions: Arc::new(RecordingSink::default()),
        operation: OperationContext {
            environment: EnvironmentId::new("local"),
            generation: Generation::new("g1"),
            owner_epoch: OwnerEpoch::LOCAL,
            operation: OperationId::new("op_1"),
            origin: Origin::Agent {
                session: session.clone(),
                run: run.clone(),
            },
        },
    }
}

#[async_trait]
impl QueryRunner for ParentRunner {
    async fn run(
        &self,
        request: QueryRequest,
        _sink: Arc<dyn RunSink>,
        _abort: Abort,
    ) -> Result<QueryStop, QueryError> {
        let tool = self.registry.find(&self.tool).unwrap();
        let (progress, _updates) = mpsc::channel(16);
        let mut context = tool_context(&request.session, &request.run);
        context.agent = request.agent.clone();
        context.env = self.env.clone();
        let output = match tool.validate_input(&self.input, &context).await {
            Validation::Invalid { message, .. } => ToolOutput {
                content: vec![ToolResultContent::Text {
                    text: format!("<tool_use_error>{message}</tool_use_error>"),
                }],
                is_error: true,
                display: Value::Null,
            },
            Validation::Ok => tool
                .call(self.input.clone(), context, progress)
                .await
                .unwrap(),
        };
        *self.output.lock().unwrap_or_else(PoisonError::into_inner) = Some(output);
        Ok(QueryStop::Completed)
    }
}

pub(crate) struct Harness {
    pub(crate) agents: Agents,
    pub(crate) store: Arc<MemoryStore>,
    pub(crate) tasks: Arc<MemoryTaskLists>,
    pub(crate) seen: Arc<Mutex<Vec<Seen>>>,
    pub(crate) absorbed: Arc<Mutex<Vec<(SessionId, String)>>>,
    /// 父运行里工具调用看到的执行环境（缺省什么都不支持）。
    pub(crate) env: Arc<dyn Environment>,
    pub(crate) _scope: TaskScope,
}

pub(crate) fn harness(gate: Option<Arc<Notify>>) -> Harness {
    built(
        gate.map(Gate::Shared),
        true,
        Arc::new(UnusedEnvironment),
        no_peers(),
    )
}

fn no_peers() -> Arc<dyn rust_cli_kernel_api::peers::PeerMessaging> {
    Arc::new(rust_cli_kernel_api::peers::NoPeers)
}

/// 同伴会话换成 `peers`（cross-session.md §1.3、§1.4）。
pub(crate) fn with_peers(peers: Arc<dyn rust_cli_kernel_api::peers::PeerMessaging>) -> Harness {
    built(None, true, Arc::new(UnusedEnvironment), peers)
}

/// 每个子会话分别放行；`keep_alive` 是交互模式（停放）还是 `-p`。
pub(crate) fn held(gates: &Gates, keep_alive: bool) -> Harness {
    held_with_env(gates, keep_alive, Arc::new(UnusedEnvironment))
}

/// 同 [`held`]，agents 自己用的执行环境换成 `env`（子会话里后台命令的通知，agents.md §5.2）。
pub(crate) fn held_with_env(gates: &Gates, keep_alive: bool, env: Arc<dyn Environment>) -> Harness {
    built(
        Some(Gate::PerSession(gates.clone())),
        keep_alive,
        env,
        no_peers(),
    )
}

/// 基础注册表按规则藏起 `hidden` 里的名字（整个工具的 deny 规则，tools-files.md §1.1.1）。
pub(crate) fn hiding(hidden: &'static [&'static str]) -> Harness {
    built_with(None, true, Arc::new(UnusedEnvironment), no_peers(), hidden)
}

/// 藏起一些名字的基础注册表：自己的工具里去掉它们，`hides` 告诉外层。
struct Hiding {
    inner: ListTools,
    hidden: &'static [&'static str],
}

impl ToolRegistry for Hiding {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let mut tools = self.inner.tools();
        tools.retain(|tool| !self.hidden.contains(&tool.name()));
        tools
    }

    fn find(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.inner
            .find(name)
            .filter(|_| !self.hidden.contains(&name))
    }

    fn hides(&self, name: &str) -> bool {
        self.hidden.contains(&name)
    }
}

fn built(
    gate: Option<Gate>,
    keep_alive: bool,
    env: Arc<dyn Environment>,
    peers: Arc<dyn rust_cli_kernel_api::peers::PeerMessaging>,
) -> Harness {
    built_with(gate, keep_alive, env, peers, &[])
}

fn built_with(
    gate: Option<Gate>,
    keep_alive: bool,
    env: Arc<dyn Environment>,
    peers: Arc<dyn rust_cli_kernel_api::peers::PeerMessaging>,
    hidden: &'static [&'static str],
) -> Harness {
    let scope = TaskScope::root("agents-test", |_| {});
    let store = Arc::new(MemoryStore::default());
    let tasks = Arc::new(MemoryTaskLists::default());
    let queries = FakeQueries {
        gate,
        ..FakeQueries::default()
    };
    let seen = queries.seen.clone();
    let absorbed = queries.absorbed.clone();
    let base: Vec<Arc<dyn Tool>> = ["Bash", "Read", "Edit", "Workflow"]
        .into_iter()
        .map(|name| Arc::new(FakeTool::new(name, true, "")) as Arc<dyn Tool>)
        .collect();
    let ports = AgentsPorts {
        store: store.clone(),
        ids: Arc::new(SequentialIds::default()),
        clock: Arc::new(FixedClock(1_000)),
        env,
        catalog: Arc::new(crate::AgentCatalog::new(Arc::new(UnusedEnvironment))),
        tasks: tasks.clone() as Arc<dyn TaskListStore>,
        queries: Arc::new(queries),
        context: Arc::new(FixedContext(Vec::new())),
        spawner: scope.spawner(),
        shell_notice: |notice| ("completed", format!("<task-id>{}</task-id>", notice.task)),
        peers,
    };
    let agents = Agents::new(
        ports,
        Arc::new(Hiding {
            inner: ListTools(base),
            hidden,
        }),
        AgentsConfig {
            task_tools: true,
            keep_alive,
            // 交互模式（停放）时后台子代理也能询问（agents.md §4.1.1）；测试的交互替身不给会话级入口。
            background_prompts: keep_alive,
        },
    );
    Harness {
        agents,
        store,
        tasks,
        seen,
        absorbed,
        env: Arc::new(UnusedEnvironment),
        _scope: scope,
    }
}

impl Harness {
    /// 在会话 `session` 的一次运行里调用 `tool`。
    pub(crate) async fn call(&self, session: &str, tool: &str, input: Value) -> ToolOutput {
        let output = Arc::new(Mutex::new(None));
        let parent = ParentRunner {
            registry: self.agents.registry(),
            env: self.env.clone(),
            tool: tool.to_owned(),
            input,
            output: output.clone(),
        };
        let runner = self.agents.track(Arc::new(parent));
        let session = SessionId::new(session);
        self.store
            .create(rust_cli_kernel_api::store::SessionMeta {
                id: session.clone(),
                kind: rust_cli_kernel_api::store::SessionKind::Interactive,
                parent: None,
                workspace: workspace(),
                title: String::new(),
                trace: rust_cli_kernel_api::ids::TraceId::new(session.as_str()),
                created_at_ms: 0,
            })
            .await
            .unwrap();
        // 子会话里的调用带子代理 ID（与主循环相同）：`sess_subagent_<id>` 里的 `<id>`。
        let agent = session
            .as_str()
            .strip_prefix("sess_subagent_")
            .map(rust_cli_kernel_api::ids::AgentId::new);
        let request = QueryRequest {
            session,
            run: RunId::new(format!("run_parent_{tool}")),
            workspace: workspace(),
            agent,
            messages: Vec::new(),
            selection: selection(),
            mode: PermissionMode::DontAsk,
            source: QuerySource::MainThread,
            max_turns: None,
            prompt_hook_messages: Vec::new(),
        };
        runner
            .run(request, Arc::new(RecordingSink::default()), Abort::new())
            .await
            .unwrap();
        output
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .unwrap()
    }

    pub(crate) fn seen(&self) -> Vec<Seen> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// 后台子代理在自己的任务里跑：让出执行权，直到子循环至少开始了 `count` 次。
    pub(crate) async fn started(&self, count: usize) -> Vec<Seen> {
        for _ in 0..10_000 {
            let seen = self.seen();
            if seen.len() >= count {
                return seen;
            }
            tokio::task::yield_now().await;
        }
        panic!("the subagent loop did not start {count} times");
    }
}

pub(crate) fn text(output: &ToolOutput) -> String {
    match output.content.as_slice() {
        [rust_cli_kernel_api::message::ToolResultContent::Text { text }] => text.clone(),
        other => panic!("unexpected content {other:?}"),
    }
}
