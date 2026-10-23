//! 自动记忆的 MEMORY.md 作为最后一个规则文件宣布（`AutoMem` 类型，memory.md §4）。

use std::sync::Arc;

use rust_cli_kernel_api::env::{EnvFacts, EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, MessageId, SessionId};
use rust_cli_kernel_api::message::{ContentBlock, Message, UserMessage, UserOrigin};
use rust_cli_kernel_api::model::{ModelSelection, QuerySource};
use rust_cli_kernel_api::orchestration::{ContextBuilder, ContextRequest};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::{FixedClock, MemoryEnvironment, SequentialIds};

use crate::{ContextConfig, ContextPorts, NoListings, PromptAssembler, Surface};

const MEMORY: &str = "/Users/dev/.zcode/projects/-Users-dev-w/memory";

fn facts() -> EnvFacts {
    EnvFacts {
        cwd: EnvPath::new("/Users/dev/w"),
        platform: "linux".to_owned(),
        os_version: "Linux 6.8.0".to_owned(),
        shell: "bash".to_owned(),
        is_git_repo: false,
        additional_directories: Vec::new(),
        home: Some(EnvPath::new("/Users/dev")),
    }
}

async fn instructions(env: MemoryEnvironment) -> Option<String> {
    let mut config = ContextConfig::new(Surface::Print);
    config.memory_dir = Some(format!("{MEMORY}/"));
    let context = PromptAssembler::new(
        ContextPorts {
            env: Arc::new(env),
            clock: Arc::new(FixedClock(1_792_224_000_000)),
            ids: Arc::new(SequentialIds::default()),
            listings: Arc::new(NoListings),
        },
        config,
    );
    let request = ContextRequest {
        capabilities: None,
        session: SessionId::new("sess_1"),
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/Users/dev/w"),
        },
        agent: None,
        source: QuerySource::MainThread,
        mode: PermissionMode::Default,
        tool_names: Vec::new(),
        selection: ModelSelection {
            provider: "anthropic".to_owned(),
            model: "claude-opus-5-5".to_owned(),
            reasoning: None,
            connection: None,
        },
        plan_flags: Default::default(),
        mcp: None,
    };
    let history = [Message::User(UserMessage {
        id: MessageId::new("u"),
        content: vec![ContentBlock::Text {
            text: "hi".to_owned(),
        }],
        origin: UserOrigin::Human,
    })];
    let built = context.build(request, &history).await.unwrap();
    built
        .attachments
        .into_iter()
        .find(|attachment| attachment.source == "instructions")
        .map(|attachment| attachment.text)
}

#[tokio::test]
async fn memory_md_comes_last_with_the_auto_memory_label() {
    let env = MemoryEnvironment::new(facts())
        .with_file("/Users/dev/w/AGENTS.md", "project rules")
        .with_file(
            &format!("{MEMORY}/MEMORY.md"),
            "\n- [Fact](fact.md) — hook\n",
        );
    let text = instructions(env).await.unwrap();
    let project = text.find("Contents of /Users/dev/w/AGENTS.md").unwrap();
    let memory = text
        .find(&format!(
            "Contents of {MEMORY}/MEMORY.md (user's auto-memory, persists across conversations):\n\n- [Fact](fact.md) — hook"
        ))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(project < memory);
}

#[tokio::test]
async fn a_missing_or_empty_memory_md_is_left_out() {
    let env = MemoryEnvironment::new(facts()).with_file(&format!("{MEMORY}/MEMORY.md"), "  \n");
    assert!(instructions(env).await.is_none());
    assert!(
        instructions(MemoryEnvironment::new(facts()))
            .await
            .is_none()
    );
}
