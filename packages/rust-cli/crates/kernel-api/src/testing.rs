//! 测试用的假端口（feature `testing`）。各 crate 的测试共用，不进生产构建。

mod clocks;
mod closing;
mod contexts;
mod cron_store;
mod env;
mod gate;
mod interactions;
mod machines;
mod memory_env;
mod memory_rewind;
mod notices;
mod overrides;
mod round_trip;
mod run;
mod scripted_model;
mod session;
mod store;
mod store_proxy;

pub use clocks::{FixedClock, ManualClock, StepClock, TokioClock};
pub use closing::ClosingEnvironment;
pub use contexts::tool_use_context;
pub use cron_store::MemoryCronStore;
pub use env::{FactsEnvironment, UnusedEnvironment};
pub use gate::FixedGate;
pub use interactions::{AskOverride, NoOneToAsk, ScriptedInteractions};
pub use machines::TestMachines;
pub use memory_env::MemoryEnvironment;
pub use notices::{NoticeEnvironment, shell_notice};
pub use overrides::{HandleOverride, RegistryOverride, SinkOverride};
pub use round_trip::RoundTripEnv;
pub use run::{DirectExecutor, FakeTool, ListTools, RecordingSink};
pub use scripted_model::ScriptedModel;
pub use session::{NoSession, OpsOverride};
pub use store::{MemoryArtifacts, MemoryStore};
pub use store_proxy::{StoreCall, StoreHooks, StoreProxy};

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::clock::Ids;
use crate::controller_services::{ControllerServices, Fetch};
use crate::env_registry::{EnvironmentRegistry, PrimaryRegistry};
use crate::ids::{
    AgentId, InteractionId, MessageId, OperationId, RunId, SessionId, ToolUseId, TurnId,
};
use crate::machine::{
    Background, Checkpoints, Commands, EnvInfo, Files, Git, MachineEnv, Pdf, Search, Shell,
};
use crate::mcp::NoMcpTransport;
use crate::message::{ContentBlock, Message, StopReason, ToolResultContent, Usage};
use crate::model::{BlockDelta, ModelError, ModelEvent, ModelSelection, SystemBlock};
use crate::orchestration::{
    BuiltContext, CompactRequest, CompactResult, Compactor, ContextBuilder, ContextRequest,
    HookEvent, HookOutcome, HookRunner, OrchestrationError, ToolExecution, ToolExecutor,
};
use crate::tool::{Tool, ToolOutput, ToolProgress, ToolUseContext};

/// 把一个实现全部 9 个机器端口的替身装成一台机器；MCP 是 [`NoMcpTransport`]（remote-exec.md §15.8.1）。
pub fn machine<E>(env: Arc<E>) -> MachineEnv
where
    E: EnvInfo + Files + Search + Commands + Shell + Background + Checkpoints + Pdf + Git + 'static,
{
    MachineEnv::new(env, Arc::new(NoMcpTransport))
}

/// 把一个实现 Fetch 的替身装成 controller 的服务。
pub fn controller_services<S>(services: Arc<S>) -> ControllerServices
where
    S: Fetch + 'static,
{
    ControllerServices::new(services)
}

/// 实现全部 10 个端口的替身（一台机器的 9 个与 controller 的 1 个）：测试的组装函数用它作约束。
pub trait AllPorts:
    EnvInfo + Files + Search + Commands + Shell + Background + Checkpoints + Pdf + Git + Fetch
{
}

impl<T> AllPorts for T where
    T: EnvInfo + Files + Search + Commands + Shell + Background + Checkpoints + Pdf + Git + Fetch
{
}

/// 只有主 env 的注册表：机器与 controller 服务都是同一个替身，MCP 是 [`NoMcpTransport`]。
pub fn environments<E: AllPorts + 'static>(env: Arc<E>) -> Arc<dyn EnvironmentRegistry> {
    Arc::new(PrimaryRegistry::new(
        machine(env.clone()),
        controller_services(env),
    ))
}

/// 每个端口都返回 `Unsupported` 的机器（[`UnusedEnvironment`]），MCP 是 [`NoMcpTransport`]。替身只实现关心的
/// 端口，再用 `MachineEnv::with_*` 换上（F126）。
pub fn unsupported_machine() -> MachineEnv {
    machine(Arc::new(UnusedEnvironment))
}

/// 每个服务都返回 `Unsupported` 的 controller（[`UnusedEnvironment`]）。
pub fn unsupported_controller() -> ControllerServices {
    controller_services(Arc::new(UnusedEnvironment))
}

/// 只有主 env `machine` 的注册表；controller 的服务都返回 `Unsupported`。
pub fn registry(machine: MachineEnv) -> Arc<dyn EnvironmentRegistry> {
    Arc::new(PrimaryRegistry::new(machine, unsupported_controller()))
}

fn start(model: &str) -> Result<ModelEvent, ModelError> {
    Ok(ModelEvent::MessageStart {
        id: "msg_api".to_owned(),
        model: model.to_owned(),
        usage: Usage::default(),
    })
}

/// 一段纯文本响应，按 `chunks` 分片流出。
pub fn text_response(chunks: &[&str]) -> Vec<Result<ModelEvent, ModelError>> {
    let mut events = vec![
        start("scripted"),
        Ok(ModelEvent::BlockStart {
            index: 0,
            block: ContentBlock::Text {
                text: String::new(),
            },
        }),
    ];
    events.extend(chunks.iter().map(|chunk| {
        Ok(ModelEvent::BlockDelta {
            index: 0,
            delta: BlockDelta::Text((*chunk).to_owned()),
        })
    }));
    events.extend([
        Ok(ModelEvent::BlockStop { index: 0 }),
        Ok(ModelEvent::MessageDelta {
            stop_reason: Some(StopReason::EndTurn),
            usage: Usage::default(),
        }),
        Ok(ModelEvent::MessageStop),
    ]);
    events
}

/// 一次 tool_use 响应。
pub fn tool_use_response(
    id: &str,
    name: &str,
    input: &Value,
) -> Vec<Result<ModelEvent, ModelError>> {
    vec![
        start("scripted"),
        Ok(ModelEvent::BlockStart {
            index: 0,
            block: ContentBlock::ToolUse {
                id: ToolUseId::new(id),
                name: name.to_owned(),
                input: Value::Object(serde_json::Map::new()),
            },
        }),
        Ok(ModelEvent::BlockDelta {
            index: 0,
            delta: BlockDelta::InputJson(input.to_string()),
        }),
        Ok(ModelEvent::BlockStop { index: 0 }),
        Ok(ModelEvent::MessageDelta {
            stop_reason: Some(StopReason::ToolUse),
            usage: Usage::default(),
        }),
        Ok(ModelEvent::MessageStop),
    ]
}

pub fn selection() -> ModelSelection {
    ModelSelection::new("fixture".to_owned(), "scripted".to_owned())
}

/// 固定 system 段、没有 attachment 的上下文。
pub struct FixedContext(pub Vec<SystemBlock>);

#[async_trait]
impl ContextBuilder for FixedContext {
    async fn build(
        &self,
        _request: ContextRequest,
        _history: &[Message],
    ) -> Result<BuiltContext, OrchestrationError> {
        Ok(BuiltContext {
            system: self.0.clone(),
            attachments: Vec::new(),
        })
    }
}

#[derive(Default)]
pub struct NoTools;

impl RegistryOverride for NoTools {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        Vec::new()
    }

    fn find(&self, _name: &str) -> Option<Arc<dyn Tool>> {
        None
    }
}

pub struct FailingExecutor;

#[async_trait]
impl ToolExecutor for FailingExecutor {
    async fn execute(
        &self,
        _tool: Arc<dyn Tool>,
        _input: Value,
        _context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> ToolExecution {
        ToolExecution::output(ToolOutput {
            content: vec![ToolResultContent::Text {
                text: "fake executor".to_owned(),
            }],
            is_error: true,
            display: Value::Null,
        })
    }
}

pub struct NeverCompact;

#[async_trait]
impl Compactor for NeverCompact {
    fn should_auto_compact(&self, _history: &[Message], _selection: &ModelSelection) -> bool {
        false
    }

    async fn compact(
        &self,
        _request: CompactRequest,
        _cancel: CancellationToken,
    ) -> Result<CompactResult, OrchestrationError> {
        Err(OrchestrationError::Unsupported {
            what: "fake compactor".to_owned(),
        })
    }
}

pub struct NoHooks;

#[async_trait]
impl HookRunner for NoHooks {
    async fn run(&self, _event: HookEvent, _cancel: CancellationToken) -> HookOutcome {
        HookOutcome::default()
    }

    fn has_hooks(&self, _event: crate::hooks::HookEventName) -> bool {
        false
    }
}

/// 可预测的 ID：`<前缀>_<序号>`。
#[derive(Default)]
pub struct SequentialIds(AtomicU64);

impl SequentialIds {
    fn next(&self, prefix: &str) -> String {
        format!("{prefix}_{}", self.0.fetch_add(1, Ordering::Relaxed) + 1)
    }
}

impl Ids for SequentialIds {
    fn session(&self) -> SessionId {
        SessionId::new(self.next("sess"))
    }

    fn subagent(&self) -> (AgentId, SessionId) {
        let id = self.next("agent");
        (
            AgentId::new(id.clone()),
            SessionId::new(format!("sess_subagent_{id}")),
        )
    }

    fn message(&self) -> MessageId {
        MessageId::new(self.next("msg"))
    }

    fn turn(&self) -> TurnId {
        TurnId::new(self.next("turn"))
    }

    fn run(&self) -> RunId {
        RunId::new(self.next("run"))
    }

    fn operation(&self) -> OperationId {
        OperationId::new(self.next("op"))
    }

    fn interaction(&self) -> InteractionId {
        InteractionId::new(self.next("interaction"))
    }

    fn command(&self) -> crate::ids::CommandId {
        crate::ids::CommandId::new(self.next("cmd"))
    }

    /// 固定值，不占序号（F7 起 runtime 建内核时取一次；与各测试夹具的 `Generation::new("g1")` 相同）。
    fn generation(&self) -> crate::ids::Generation {
        crate::ids::Generation::new("g1")
    }
}
