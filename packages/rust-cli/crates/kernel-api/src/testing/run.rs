//! 主循环输出端与工具的测试替身。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::{Notify, mpsc};

use super::lock;
use crate::ids::{InteractionId, OperationId, SessionId};
use crate::message::{Message, ToolResultContent};
use crate::orchestration::{
    InteractionAnswer, InteractionRequest, Interactions, OrchestrationError, QueryEvent, RunSink,
    ToolExecutor, ToolRegistry,
};
use crate::store::StoreError;
use crate::tool::{
    InterruptBehavior, PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext,
};

/// 记录提交的消息、意图与事件；`ask` 一律拒绝。
#[derive(Default)]
pub struct RecordingSink {
    messages: Mutex<Vec<Message>>,
    intents: Mutex<Vec<(OperationId, String)>>,
    events: Mutex<Vec<QueryEvent>>,
    steering: Mutex<Vec<String>>,
    /// `commit_metered` 的计量，按提交顺序，带同批消息的 id（usage.md §2.5）。
    metered: Mutex<Vec<(Option<crate::ids::MessageId>, crate::metering::Metered)>>,
}

impl RecordingSink {
    /// 下一次 `take_steering` 取走的插话。
    pub fn steer(&self, text: &str) {
        lock(&self.steering).push(text.to_owned());
    }

    pub fn messages(&self) -> Vec<Message> {
        lock(&self.messages).clone()
    }

    pub fn intents(&self) -> Vec<(OperationId, String)> {
        lock(&self.intents).clone()
    }

    pub fn events(&self) -> Vec<QueryEvent> {
        lock(&self.events).clone()
    }

    pub fn metered(&self) -> Vec<(Option<crate::ids::MessageId>, crate::metering::Metered)> {
        lock(&self.metered).clone()
    }
}

#[async_trait]
impl Interactions for RecordingSink {
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
impl RunSink for RecordingSink {
    async fn commit_message(&self, message: Message) -> Result<(), StoreError> {
        lock(&self.messages).push(message);
        Ok(())
    }

    async fn commit_intent(
        &self,
        operation: OperationId,
        fingerprint: String,
    ) -> Result<(), StoreError> {
        lock(&self.intents).push((operation, fingerprint));
        Ok(())
    }

    async fn commit_metered(
        &self,
        message: Option<Message>,
        metered: crate::metering::Metered,
    ) -> Result<(), StoreError> {
        lock(&self.metered).push((message.as_ref().map(|m| m.id().clone()), metered));
        if let Some(message) = message {
            lock(&self.messages).push(message);
        }
        Ok(())
    }

    async fn take_steering(&self) -> Vec<String> {
        std::mem::take(&mut *lock(&self.steering))
    }

    async fn emit(&self, event: QueryEvent) -> bool {
        lock(&self.events).push(event);
        true
    }
}

/// 可控的工具：等 `gate` 放行（或被取消）后返回 `output`；`started` 在开始执行时通知。
pub struct FakeTool {
    pub name: String,
    pub safe: bool,
    pub interrupt: InterruptBehavior,
    pub output: ToolOutput,
    pub gate: Option<Arc<Notify>>,
    pub started: Arc<Notify>,
}

impl FakeTool {
    pub fn new(name: &str, safe: bool, text: &str) -> Self {
        Self {
            name: name.to_owned(),
            safe,
            interrupt: InterruptBehavior::Block,
            output: ToolOutput {
                content: vec![ToolResultContent::Text {
                    text: text.to_owned(),
                }],
                is_error: false,
                display: Value::Null,
            },
            gate: None,
            started: Arc::new(Notify::new()),
        }
    }

    pub fn failing(mut self) -> Self {
        self.output.is_error = true;
        self
    }

    pub fn gated(mut self, gate: Arc<Notify>) -> Self {
        self.gate = Some(gate);
        self
    }
}

#[async_trait]
impl Tool for FakeTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({"type": "object"})
    }

    fn description(&self, _context: &PromptContext) -> String {
        format!("{} test tool", self.name)
    }

    fn is_read_only(&self, _input: &Value) -> bool {
        self.safe
    }

    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        self.safe
    }

    fn interrupt_behavior(&self) -> InterruptBehavior {
        self.interrupt
    }

    async fn call(
        &self,
        _input: Value,
        context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        self.started.notify_one();
        if let Some(gate) = &self.gate {
            tokio::select! {
                () = gate.notified() => {}
                () = context.cancel.cancelled() => return Err(ToolError::Cancelled),
            }
        }
        Ok(self.output.clone())
    }
}

/// 直接调用工具的执行管线（不做校验、权限与 hooks）。
pub struct DirectExecutor;

#[async_trait]
impl ToolExecutor for DirectExecutor {
    async fn execute(
        &self,
        tool: Arc<dyn Tool>,
        input: Value,
        context: ToolUseContext,
        progress: mpsc::Sender<ToolProgress>,
    ) -> ToolOutput {
        match tool.call(input, context, progress).await {
            Ok(output) => output,
            Err(error) => ToolOutput {
                content: vec![ToolResultContent::Text {
                    text: format!("<tool_use_error>{error}</tool_use_error>"),
                }],
                is_error: true,
                display: Value::Null,
            },
        }
    }
}

/// 固定的工具列表。
pub struct ListTools(pub Vec<Arc<dyn Tool>>);

impl ToolRegistry for ListTools {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.0.clone()
    }

    fn find(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.0.iter().find(|tool| tool.name() == name).cloned()
    }
}
