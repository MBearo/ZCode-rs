//! 测试共用：按顺序回答摘要请求的假子循环、消息构造。

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::compaction::PostCompactContext;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, Generation, MessageId, SessionId, ToolUseId};
use rust_cli_kernel_api::message::{
    AssistantMessage, AttachmentMessage, CompactTrigger, ContentBlock, Message, ToolResultContent,
    Usage, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::model::{ModelError, ModelSelection};
use rust_cli_kernel_api::orchestration::{
    CompactRequest, ContextRequest, OrchestrationError, QueryError, QueryRequest, QueryRunner,
    QueryStop, RunSink, ToolRegistry,
};
use rust_cli_kernel_api::testing::{
    ListTools, MemoryStore, NoHooks, SequentialIds, UnusedEnvironment, local_services, machine,
};
use rust_cli_kernel_api::tool::ReadFileStates;
use serde_json::json;

use crate::CompactPorts;

/// 摘要子循环的一次回答。
pub(crate) enum Reply {
    Text(String),
    PromptTooLong(String),
}

#[derive(Default)]
pub(crate) struct ScriptedRunner {
    replies: Mutex<Vec<Reply>>,
    pub(crate) requests: Mutex<Vec<QueryRequest>>,
}

impl ScriptedRunner {
    pub(crate) fn new(replies: Vec<Reply>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into_iter().rev().collect()),
            requests: Mutex::default(),
        })
    }

    pub(crate) fn requests(&self) -> Vec<QueryRequest> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl QueryRunner for ScriptedRunner {
    async fn run(
        &self,
        request: QueryRequest,
        sink: Arc<dyn RunSink>,
        _abort: Abort,
    ) -> Result<QueryStop, QueryError> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(request);
        let reply = self
            .replies
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop()
            .expect("no scripted reply left");
        match reply {
            Reply::Text(text) => {
                sink.commit_message(Message::Assistant(AssistantMessage {
                    id: MessageId::new("summary_reply"),
                    content: vec![ContentBlock::Text { text }],
                    model: "m".to_owned(),
                    stop_reason: None,
                    usage: None,
                }))
                .await?;
                Ok(QueryStop::Completed)
            }
            Reply::PromptTooLong(message) => {
                Err(QueryError::Model(ModelError::PromptTooLong { message }))
            }
        }
    }
}

pub(crate) fn workspace() -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new("/Users/dev/project"),
    }
}

/// 假的 readFileState：`clear` 交出记着的路径（最近的在前）并记下清空了哪个会话。
#[derive(Default)]
pub(crate) struct FakeFileStates {
    pub(crate) recent: Mutex<Vec<String>>,
    pub(crate) cleared: Mutex<Vec<SessionId>>,
}

impl FakeFileStates {
    pub(crate) fn with(recent: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            recent: Mutex::new(recent.iter().map(|path| (*path).to_owned()).collect()),
            cleared: Mutex::default(),
        })
    }
}

impl ReadFileStates for FakeFileStates {
    fn clear(&self, session: &SessionId) -> Vec<String> {
        self.cleared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(session.clone());
        std::mem::take(&mut self.recent.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// 没有清单增量的 context。
pub(crate) struct NoListings;

#[async_trait]
impl PostCompactContext for NoListings {
    async fn post_compact(
        &self,
        _request: ContextRequest,
        _kept: &[Message],
    ) -> Result<Vec<AttachmentMessage>, OrchestrationError> {
        Ok(Vec::new())
    }
}

pub(crate) fn ports(runner: Arc<dyn QueryRunner>, tools: Arc<dyn ToolRegistry>) -> CompactPorts {
    ports_with_files(runner, tools, Arc::new(FakeFileStates::default()))
}

pub(crate) fn ports_with_files(
    runner: Arc<dyn QueryRunner>,
    tools: Arc<dyn ToolRegistry>,
    files: Arc<dyn ReadFileStates>,
) -> CompactPorts {
    let store = Arc::new(MemoryStore::default());
    let env = Arc::new(UnusedEnvironment);
    CompactPorts {
        runner,
        tools,
        files,
        context: Arc::new(NoListings),
        hooks: Arc::new(NoHooks),
        ids: Arc::new(SequentialIds::default()),
        machine: machine(env.clone()),
        local: local_services(env),
        artifacts: store,
        generation: Generation::new("g"),
    }
}

pub(crate) fn no_tools() -> Arc<dyn ToolRegistry> {
    Arc::new(ListTools(Vec::new()))
}

pub(crate) fn request(messages: Vec<Message>, trigger: CompactTrigger) -> CompactRequest {
    CompactRequest {
        session: SessionId::new("s"),
        workspace: workspace(),
        trigger,
        selection: ModelSelection {
            provider: "anthropic".to_owned(),
            model: rust_cli_base::models::GLM_5_3.to_owned(),
            reasoning: None,
            connection: None,
        },
        instructions: None,
        messages,
        mode: None,
        agent: None,
        initial_token_gap: None,
    }
}

pub(crate) fn human(id: &str, text: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        origin: UserOrigin::Human,
    })
}

pub(crate) fn reply(id: &str, text: &str, usage: Option<Usage>) -> Message {
    Message::Assistant(AssistantMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        model: "m".to_owned(),
        stop_reason: None,
        usage,
    })
}

/// 一轮工具调用：assistant（tool_use）+ 结果消息。
pub(crate) fn tool_round(index: usize, tool: &str, path: &str) -> [Message; 2] {
    let id = ToolUseId::new(format!("toolu_{index}"));
    let assistant = format!("a_tool_{index}");
    [
        Message::Assistant(AssistantMessage {
            id: MessageId::new(&assistant),
            content: vec![ContentBlock::ToolUse {
                id: id.clone(),
                name: tool.to_owned(),
                input: json!({ "file_path": path }),
            }],
            model: "m".to_owned(),
            stop_reason: None,
            usage: None,
        }),
        Message::User(UserMessage {
            id: MessageId::new(format!("r_tool_{index}")),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: id,
                content: vec![ToolResultContent::Text {
                    text: "ok".to_owned(),
                }],
                is_error: false,
            }],
            origin: UserOrigin::ToolResult {
                assistant: MessageId::new(assistant),
            },
        }),
    ]
}

pub(crate) fn texts(message: &Message) -> String {
    let blocks = match message {
        Message::User(user) => &user.content,
        Message::Assistant(assistant) => &assistant.content,
        Message::Attachment(attachment) => return attachment.text.clone(),
        Message::System(_) => return String::new(),
    };
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("|")
}
