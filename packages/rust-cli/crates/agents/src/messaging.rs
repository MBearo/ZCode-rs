//! ListAgents、SendMessage（docs/specs/rust-cli/agents.md §6）：本进程里启动过的子代理与本机的同伴会话
//! （docs/specs/rust-cli/cross-session.md，F28；段落与同伴寻址在 `peer_send`）。描述与 schema 在 `prompts/tools/`
//! （ListAgents、SendMessage），删掉了不实的产品句（cross-session.md §2.7）。TaskStop 在 `task_stop`。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::ids::AgentId;
use rust_cli_kernel_api::permission::PermissionDecision;
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext,
};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::Core;
use crate::agent_tool::text_output;
use crate::spawn;
use crate::task_stop::TaskStopTool;

pub(crate) fn tools(core: &Arc<Core>) -> Vec<Arc<dyn Tool>> {
    let mut tools: Vec<Arc<dyn Tool>> = [Kind::ListAgents, Kind::SendMessage]
        .into_iter()
        .map(|kind| {
            Arc::new(Messaging {
                core: core.clone(),
                kind,
            }) as Arc<dyn Tool>
        })
        .collect();
    tools.push(Arc::new(TaskStopTool { core: core.clone() }));
    tools
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    ListAgents,
    SendMessage,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::ListAgents => "ListAgents",
            Self::SendMessage => "SendMessage",
        }
    }
}

struct Messaging {
    core: Arc<Core>,
    kind: Kind,
}

#[derive(Deserialize)]
struct SendInput {
    to: String,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    message: String,
}

impl Messaging {
    /// 段落格式：自己那一段、子代理、同伴会话（cross-session.md §1.3）。
    async fn list(&self, context: &ToolUseContext) -> ToolOutput {
        text_output(crate::peer_send::listing(&self.core, context).await, false)
    }

    async fn send(
        &self,
        input: SendInput,
        context: &ToolUseContext,
    ) -> Result<ToolOutput, ToolError> {
        let id = AgentId::new(input.to.trim());
        let known = self
            .core
            .live
            .get(&id)
            .filter(|agent| agent.parent == context.session);
        let Some(agent) = known else {
            // 进程内没有：按本机的同伴会话寻址（cross-session.md §1.4）。都没有时是打开跨会话后的
            // `No agent named '<to>' is reachable.`（结果 `success: false`、不标 is_error）。以前是关闭跨会话时的
            // `… is currently addressable …`，并标 is_error。
            let text = crate::peer_send::send(
                &self.core,
                &input.to,
                input.summary.as_deref(),
                &input.message,
                context,
            )
            .await;
            return Ok(text_output(text, false));
        };
        if self.core.live.enqueue(&id, input.message.clone()) {
            let text = format!(
                "Message queued for delivery to {} at its next tool round.",
                input.to
            );
            return Ok(text_output(text, false));
        }
        // 已结束：在后台续跑子会话。
        let status = agent.status();
        if !self.core.live.restart(&id, Abort::new()) {
            // 并发的另一条消息刚刚让它跑起来：排进邮箱。
            self.core.live.enqueue(&id, input.message.clone());
            let text = format!(
                "Message queued for delivery to {} at its next tool round.",
                input.to
            );
            return Ok(text_output(text, false));
        }
        let origin = rust_cli_kernel_api::message::UserOrigin::Human;
        let opening = match spawn::resume(&self.core, &agent, &input.message, origin).await {
            Ok(opening) => opening,
            Err(error) => {
                self.core.live.finish(&id, crate::report::Ending::Failed);
                return Err(ToolError::Failed {
                    message: error.to_string(),
                });
            }
        };
        let Some(running) = self.core.live.get(&id) else {
            return Err(ToolError::Failed {
                message: "the agent disappeared".to_owned(),
            });
        };
        spawn::background(&self.core, running, opening);
        let text = format!(
            "Agent \"{}\" was stopped ({status}); it was resumed in the background with your message. You'll be notified when it finishes.",
            input.to
        );
        Ok(text_output(text, false))
    }
}

#[async_trait]
impl Tool for Messaging {
    fn name(&self) -> &str {
        self.kind.name()
    }

    /// 延迟加载（docs/specs/rust-cli/tools-more.md §3.1）：ListAgents 内联，SendMessage 延迟。
    fn should_defer(&self) -> bool {
        self.kind == Kind::SendMessage
    }

    /// ToolSearch 的关键词提示。
    fn search_hint(&self) -> Option<&str> {
        Some(match self.kind {
            Kind::ListAgents => "list agents you can SendMessage to",
            Kind::SendMessage => "send messages to agent teammates",
        })
    }

    fn input_schema(&self) -> Value {
        rust_cli_prompts::tool(self.kind.name())
            .map_or(Value::Null, |prompt| prompt.input_schema.clone())
    }

    fn description(&self, _context: &PromptContext) -> String {
        rust_cli_prompts::tool(self.kind.name())
            .map_or_else(String::new, |prompt| prompt.description.clone())
    }

    fn is_read_only(&self, _input: &Value) -> bool {
        self.kind == Kind::ListAgents
    }

    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        self.kind == Kind::ListAgents
    }

    async fn check_permissions(
        &self,
        _input: &Value,
        _context: &ToolUseContext,
    ) -> PermissionDecision {
        PermissionDecision::Allow {
            updated_input: None,
        }
    }

    async fn call(
        &self,
        value: Value,
        context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let invalid = |error: serde_json::Error| ToolError::InvalidInput {
            message: error.to_string(),
        };
        match self.kind {
            Kind::ListAgents => Ok(self.list(&context).await),
            Kind::SendMessage => {
                let input = serde_json::from_value(value).map_err(invalid)?;
                self.send(input, &context).await
            }
        }
    }
}
