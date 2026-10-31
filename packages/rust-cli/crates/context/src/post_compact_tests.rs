//! 压缩产生的清单增量（compact.md §3.4）与压缩后 skill 清单不重发（context.md §4）。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::compaction::PostCompactContext;
use rust_cli_kernel_api::env::{EnvFacts, EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, MessageId, SessionId};
use rust_cli_kernel_api::message::{
    CompactBoundary, CompactTrigger, ContentBlock, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::{FixedClock, MemoryEnvironment, SequentialIds};
use serde_json::json;

use super::*;

struct Fixed;

fn entry(name: &str) -> ListingEntry {
    ListingEntry {
        name: name.to_owned(),
        line: format!("- {name}"),
    }
}

#[async_trait]
impl ListingSource for Fixed {
    async fn listings(&self, _request: &ContextRequest) -> Result<Listings, OrchestrationError> {
        Ok(Listings {
            deferred_tools: vec![entry("WebFetch")],
            agents: vec![entry("Explore")],
            skills: vec![entry("review")],
        })
    }

    async fn mcp_instructions(
        &self,
        _request: &ContextRequest,
    ) -> Result<Vec<ListingEntry>, OrchestrationError> {
        Ok(vec![ListingEntry {
            name: "docs".to_owned(),
            line: "## docs\nUse the docs server.".to_owned(),
        }])
    }
}

fn assembler() -> PromptAssembler {
    let facts = EnvFacts {
        cwd: EnvPath::new("/Users/dev/w"),
        platform: "linux".to_owned(),
        os_version: "Linux 6.8.0".to_owned(),
        shell: "bash".to_owned(),
        is_git_repo: false,
        additional_directories: Vec::new(),
        home: Some(EnvPath::new("/Users/dev")),
    };
    let env = Arc::new(MemoryEnvironment::new(facts));
    PromptAssembler::new(
        ContextPorts {
            info: env.clone(),
            files: env.clone(),
            shell: env.clone(),
            git: env,
            clock: Arc::new(FixedClock(1_792_224_000_000)),
            ids: Arc::new(SequentialIds::default()),
            listings: Arc::new(Fixed),
        },
        ContextConfig::new(Surface::Print),
    )
}

fn request(source: QuerySource) -> ContextRequest {
    ContextRequest {
        default_machine: None,
        capabilities: None,
        session: SessionId::new("s"),
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/Users/dev/w"),
        },
        agent: None,
        source,
        mode: PermissionMode::Default,
        tool_names: vec!["Read".to_owned(), "Agent".to_owned(), "Skill".to_owned()],
        selection: ModelSelection {
            provider: "p".to_owned(),
            model: rust_cli_base::models::GLM_5_3.to_owned(),
            reasoning: None,
            connection: None,
        },
        plan_flags: Default::default(),
        mcp: None,
    }
}

fn sources(items: &[AttachmentMessage]) -> Vec<&str> {
    items.iter().map(|item| item.source.as_str()).collect()
}

fn human(id: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text {
            text: "hi".to_owned(),
        }],
        origin: UserOrigin::Human,
    })
}

/// 压缩后的历史：之前的回合（宣布过全部清单），边界，摘要，压缩附件。
async fn compacted(assembler: &PromptAssembler) -> Vec<Message> {
    let mut history = vec![human("msg_u1")];
    let first = assembler
        .step(request(QuerySource::MainThread), &history)
        .await
        .unwrap();
    history.extend(first.attachments.into_iter().map(Message::Attachment));
    history.push(Message::System(SystemMessage::CompactBoundary(
        CompactBoundary {
            id: MessageId::new("msg_boundary"),
            trigger: CompactTrigger::Reactive,
            pre_tokens: 1,
            summary: MessageId::new("msg_summary"),
            preserved: None,
            discovered_tools: Vec::new(),
        },
    )));
    history.push(Message::User(UserMessage {
        id: MessageId::new("msg_summary"),
        content: vec![ContentBlock::Text {
            text: "summary".to_owned(),
        }],
        origin: UserOrigin::CompactSummary,
    }));
    let restored = assembler
        .post_compact(request(QuerySource::MainThread), &[])
        .await
        .unwrap();
    history.extend(restored.into_iter().map(Message::Attachment));
    history
}

/// 主线程：延迟工具、agent、MCP instructions，再是环境段与模型身份，顺序同压缩附件；按保留尾段算增量。
/// F10 之前环境段与模型身份由压缩后下一步的上下文宣布。
#[tokio::test]
async fn compaction_attaches_the_listing_deltas_before_the_environment() {
    let assembler = assembler();
    let restored = assembler
        .post_compact(request(QuerySource::MainThread), &[])
        .await
        .unwrap();
    assert_eq!(
        sources(&restored),
        [
            "deferred_tools_delta",
            "agent_listing_delta",
            "mcp_instructions_delta",
            "environment",
            "model"
        ]
    );
    let kept = vec![Message::Attachment(AttachmentMessage {
        id: MessageId::new("msg_kept"),
        source: "deferred_tools_delta".to_owned(),
        text: String::new(),
        data: json!({"addedNames": ["WebFetch"]}),
    })];
    let restored = assembler
        .post_compact(request(QuerySource::MainThread), &kept)
        .await
        .unwrap();
    assert_eq!(
        sources(&restored),
        [
            "agent_listing_delta",
            "mcp_instructions_delta",
            "environment",
            "model"
        ]
    );
}

/// 压缩后的下一步（自动压缩之后的上下文宣布）：清单、环境段与模型身份已在压缩附件里，不再宣布；日期照常
/// 重新宣布（顺序：文件、清单、环境、模型、日期）。
#[tokio::test]
async fn the_next_step_after_compaction_does_not_repeat_the_listings() {
    let assembler = assembler();
    let history = compacted(&assembler).await;
    let next = assembler
        .build(request(QuerySource::MainThread), &history)
        .await
        .unwrap();
    assert_eq!(sources(&next.attachments), ["date"]);
}

/// 宣布过的 skill 跨压缩保留：压缩后不重发；新出现的 skill 照常宣布。
#[tokio::test]
async fn announced_skills_survive_compaction() {
    let assembler = assembler();
    let history = compacted(&assembler).await;
    assert!(
        history.iter().any(|message| matches!(
            message,
            Message::Attachment(attachment) if attachment.source == "skill_listing"
        )),
        "the first step announces the skills"
    );
    // 压缩之后的下一条人类输入：当步的 attachment 里没有 skill 清单。
    let mut history = history;
    history.push(human("msg_u2"));
    let next = assembler
        .step(request(QuerySource::MainThread), &history)
        .await
        .unwrap();
    let announced = sources(&next.attachments);
    assert!(!announced.contains(&"skill_listing"), "{announced:?}");
}
