//! 摘要请求的宣布（context.md §4.1，上下文宣布的 auxiliary 分支）。

use std::sync::Arc;

use rust_cli_kernel_api::env::{EnvFacts, EnvPath, WorkspaceRef};
use rust_cli_kernel_api::env_git::{GitSnapshot, GitStatus};
use rust_cli_kernel_api::ids::{EnvironmentId, MessageId, SessionId};
use rust_cli_kernel_api::message::{ContentBlock, UserMessage, UserOrigin};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::{FixedClock, MemoryEnvironment, SequentialIds};

use super::*;

fn environment() -> MemoryEnvironment {
    MemoryEnvironment::new(EnvFacts {
        cwd: EnvPath::new("/Users/dev/w"),
        platform: "linux".to_owned(),
        os_version: "Linux 6.8.0".to_owned(),
        shell: "bash".to_owned(),
        is_git_repo: true,
        additional_directories: Vec::new(),
        home: Some(EnvPath::new("/Users/dev")),
    })
    .with_file("/Users/dev/w/AGENTS.md", "project rules")
    .with_git(GitStatus::Repository(GitSnapshot {
        branch: "main".to_owned(),
        default_branch: "main".to_owned(),
        status: String::new(),
        recent_commits: "abc1234 first".to_owned(),
        user_name: None,
    }))
}

fn assembler() -> PromptAssembler {
    let env = Arc::new(environment());
    PromptAssembler::new(
        ContextPorts {
            info: env.clone(),
            files: env.clone(),
            shell: env.clone(),
            git: env,
            clock: Arc::new(FixedClock(1_792_224_000_000)),
            ids: Arc::new(SequentialIds::default()),
            listings: Arc::new(NoListings),
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
        tool_names: vec!["Read".to_owned(), "Bash".to_owned()],
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

fn user(id: &str, text: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        origin: UserOrigin::Human,
    })
}

fn sources(built: &BuiltContext) -> Vec<&str> {
    built
        .attachments
        .iter()
        .map(|item| item.source.as_str())
        .collect()
}

/// 裁掉开头后：消息里没有规则文件、会话上下文与日期，按上下文宣布的顺序重新宣布；
/// 环境段、模型、剩余 token、署名不发。
#[tokio::test]
async fn a_truncated_summary_request_announces_what_it_lost() {
    let history = vec![user("msg_marker", "[earlier conversation truncated]")];
    let built = assembler()
        .build(request(QuerySource::Compact), &history)
        .await
        .unwrap();
    assert_eq!(sources(&built), ["instructions", "session_context", "date"]);
    assert!(built.attachments[0].text.contains("project rules"));
}

/// 要摘要的部分里已经有这些宣布（没有裁头）：什么也不发。
#[tokio::test]
async fn an_untruncated_summary_request_announces_nothing() {
    let assembler = assembler();
    let main = assembler
        .build(request(QuerySource::MainThread), &[user("msg_u", "hi")])
        .await
        .unwrap();
    let mut history = vec![user("msg_u", "hi")];
    history.extend(main.attachments.into_iter().map(Message::Attachment));
    let built = assembler
        .build(request(QuerySource::Compact), &history)
        .await
        .unwrap();
    assert!(built.attachments.is_empty(), "{:?}", sources(&built));
}

/// 旁路请求仍然什么也不发。
#[tokio::test]
async fn side_requests_announce_nothing() {
    let built = assembler()
        .build(request(QuerySource::Side("title".to_owned())), &[])
        .await
        .unwrap();
    assert!(built.attachments.is_empty());
}
