//! 测试用的假端口（feature `testing`）。各 crate 的测试共用，不进生产构建。

mod closing;
mod cron_store;
mod env;
mod interactions;
mod memory_env;
mod memory_rewind;
mod notices;
mod round_trip;
mod run;
mod store;

pub use closing::ClosingEnvironment;
pub use cron_store::MemoryCronStore;
pub use env::{FactsEnvironment, UnusedEnvironment};
pub use interactions::ScriptedInteractions;
pub use memory_env::MemoryEnvironment;
pub use notices::{NoticeEnvironment, shell_notice};
pub use round_trip::RoundTripEnv;
pub use run::{DirectExecutor, FakeTool, ListTools, RecordingSink};
pub use store::{MemoryArtifacts, MemoryStore};

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::clock::{Clock, Ids};
use crate::ids::{
    AgentId, InteractionId, MessageId, OperationId, RunId, SessionId, ToolUseId, TurnId,
};
use crate::message::{ContentBlock, Message, StopReason, ToolResultContent, Usage};
use crate::model::{
    BlockDelta, ModelError, ModelEvent, ModelPort, ModelRequest, ModelSelection, SystemBlock,
};
use crate::orchestration::{
    BuiltContext, CompactRequest, CompactResult, Compactor, ContextBuilder, ContextRequest,
    HookEvent, HookOutcome, HookRunner, OrchestrationError, ToolExecutor, ToolRegistry,
};
use crate::tool::{Tool, ToolOutput, ToolProgress, ToolUseContext};

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 按顺序返回预先写好的响应，并记录收到的请求。
#[derive(Default)]
pub struct ScriptedModel {
    responses: Mutex<VecDeque<Vec<Result<ModelEvent, ModelError>>>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl ScriptedModel {
    pub fn new(responses: Vec<Vec<Result<ModelEvent, ModelError>>>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            requests: Mutex::default(),
        }
    }

    pub fn requests(&self) -> Vec<ModelRequest> {
        lock(&self.requests).clone()
    }
}

impl ModelPort for ScriptedModel {
    fn stream(
        &self,
        request: ModelRequest,
        _cancel: CancellationToken,
    ) -> BoxStream<'static, Result<ModelEvent, ModelError>> {
        lock(&self.requests).push(request);
        let events = lock(&self.responses).pop_front().unwrap_or_else(|| {
            vec![Err(ModelError::Protocol {
                message: "no scripted response left".to_owned(),
            })]
        });
        stream::iter(events).boxed()
    }
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
    ModelSelection {
        provider: "fixture".to_owned(),
        model: "scripted".to_owned(),
        reasoning: None,
        connection: None,
    }
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
            user_context: Vec::new(),
            attachments: Vec::new(),
        })
    }
}

#[derive(Default)]
pub struct NoTools;

impl ToolRegistry for NoTools {
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
    ) -> ToolOutput {
        ToolOutput {
            content: vec![ToolResultContent::Text {
                text: "fake executor".to_owned(),
            }],
            is_error: true,
            display: Value::Null,
        }
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

pub struct FixedClock(pub u64);

impl Clock for FixedClock {
    fn now_ms(&self) -> u64 {
        self.0
    }

    fn time_zone(&self) -> String {
        "UTC".to_owned()
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
