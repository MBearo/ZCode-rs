//! 规则文件、nested memory 与会话上下文经 [`PromptAssembler`] 的整体行为（context.md §11、§12）。

use std::sync::Arc;

use rust_cli_kernel_api::env::{EnvFacts, EnvPath, WorkspaceRef};
use rust_cli_kernel_api::env_git::{GitSnapshot, GitStatus};
use rust_cli_kernel_api::ids::{EnvironmentId, MessageId, SessionId, ToolUseId};
use rust_cli_kernel_api::message::{
    AssistantMessage, AttachmentRole, CompactBoundary, CompactTrigger, ContentBlock,
    ToolResultContent, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::{FixedClock, MemoryEnvironment, SequentialIds};
use serde_json::json;

use super::*;

fn facts() -> EnvFacts {
    EnvFacts {
        cwd: EnvPath::new("/Users/dev/w"),
        platform: "linux".to_owned(),
        os_version: "Linux 6.8.0".to_owned(),
        shell: "bash".to_owned(),
        is_git_repo: true,
        additional_directories: Vec::new(),
        home: Some(EnvPath::new("/Users/dev")),
    }
}

fn environment() -> MemoryEnvironment {
    MemoryEnvironment::new(facts())
        .with_file("/Users/dev/.zcode/AGENTS.md", "user rules")
        .with_file("/Users/dev/w/AGENTS.md", "project rules @docs/style.md")
        .with_file("/Users/dev/w/docs/style.md", "style guide")
        .with_file("/Users/dev/w/src/AGENTS.md", "src rules")
        .with_git(GitStatus::Repository(GitSnapshot {
            branch: "main".to_owned(),
            default_branch: "main".to_owned(),
            status: String::new(),
            recent_commits: "abc1234 first".to_owned(),
            user_name: None,
        }))
}

fn assembler(env: Arc<MemoryEnvironment>) -> PromptAssembler {
    PromptAssembler::new(
        ContextPorts {
            env,
            clock: Arc::new(FixedClock(1_792_224_000_000)),
            ids: Arc::new(SequentialIds::default()),
            listings: Arc::new(NoListings),
        },
        ContextConfig::new(Surface::Print),
    )
}

fn request() -> ContextRequest {
    ContextRequest {
        capabilities: None,
        session: SessionId::new("s"),
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/Users/dev/w"),
        },
        agent: None,
        source: QuerySource::MainThread,
        mode: PermissionMode::Default,
        tool_names: vec!["Read".to_owned()],
        selection: ModelSelection {
            provider: "p".to_owned(),
            model: "claude-opus-5-5".to_owned(),
            reasoning: None,
            connection: None,
        },
        plan_flags: Default::default(),
        mcp: None,
    }
}

fn human() -> Message {
    Message::User(UserMessage {
        id: MessageId::new("prompt"),
        content: vec![ContentBlock::Text {
            text: "hi".to_owned(),
        }],
        origin: UserOrigin::Human,
    })
}

fn sources(attachments: &[AttachmentMessage]) -> Vec<&str> {
    attachments
        .iter()
        .map(|item| item.source.as_str())
        .collect()
}

/// 跑一步：把产生的 attachment 追加进历史（query 落库后才继续）。
async fn step(context: &PromptAssembler, history: &mut Vec<Message>) -> Vec<AttachmentMessage> {
    let built = context.step(request(), history).await.unwrap();
    history.extend(built.attachments.iter().cloned().map(Message::Attachment));
    built.attachments
}

#[tokio::test]
async fn the_first_step_announces_rules_before_the_git_status() {
    let env = Arc::new(environment());
    let context = assembler(env.clone());
    let mut history = vec![human()];
    let first = step(&context, &mut history).await;
    assert_eq!(
        sources(&first),
        [
            "environment",
            "model",
            "total_tokens_reminder",
            "instructions",
            "session_context",
            "date",
        ]
    );
    let instructions = &first[3];
    assert_eq!(instructions.role(), AttachmentRole::User);
    assert_eq!(first[4].role(), AttachmentRole::User);
    let paths: Vec<&str> = instructions.data["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        paths,
        [
            "/Users/dev/.zcode/AGENTS.md",
            "/Users/dev/w/AGENTS.md",
            "/Users/dev/w/docs/style.md"
        ]
    );
    assert!(
        instructions
            .text
            .starts_with("<system-reminder>\nCodebase and user instructions")
    );
    assert!(
        first[4]
            .text
            .contains("# gitStatus\nThis is the git status")
    );

    // 下一步：缓存命中，不再读文件；已宣布的规则与会话上下文不重发。
    let reads = env.reads();
    let second = step(&context, &mut history).await;
    assert_eq!(sources(&second), ["total_tokens_reminder"]);
    assert_eq!(env.reads(), reads);

    // 同一进程里改了文件：有缓存，不会注意到。
    env.set_file("/Users/dev/w/AGENTS.md", "edited");
    assert_eq!(
        sources(&step(&context, &mut history).await),
        ["total_tokens_reminder"]
    );

    // 新进程（resume）重读一次，只宣布差异。
    let resumed = assembler(env.clone());
    let delta = step(&resumed, &mut history).await;
    assert_eq!(sources(&delta), ["total_tokens_reminder", "instructions"]);
    assert_eq!(delta[1].data["changed"], true);
    assert_eq!(delta[1].data["reason"], "session_start");
    assert_eq!(
        delta[1].data["files"],
        json!([{"path": "/Users/dev/w/AGENTS.md", "type": "Project", "content": "edited"}])
    );
    // 原来由它 include 的文件不再出现，算作撤回。
    assert_eq!(
        delta[1].data["removed"],
        json!(["/Users/dev/w/docs/style.md"])
    );
}

#[tokio::test]
async fn a_compaction_boundary_re_reads_and_announces_everything_again() {
    let env = Arc::new(environment());
    let context = assembler(env.clone());
    let mut history = vec![human()];
    step(&context, &mut history).await;
    history.push(Message::System(SystemMessage::CompactBoundary(
        CompactBoundary {
            id: MessageId::new("boundary"),
            trigger: CompactTrigger::Auto,
            pre_tokens: 1,
            summary: MessageId::new("summary"),
            preserved: None,
        },
    )));
    let reads = env.reads();
    let after = step(&context, &mut history).await;
    assert!(env.reads() > reads);
    let after: Vec<&str> = sources(&after);
    assert!(after.contains(&"instructions"));
    assert!(after.contains(&"session_context"));
}

#[tokio::test]
async fn reading_a_file_in_a_subdirectory_brings_its_rules_once() {
    let env = Arc::new(environment());
    let context = assembler(env);
    let mut history = vec![human()];
    step(&context, &mut history).await;
    history.push(Message::Assistant(AssistantMessage {
        id: MessageId::new("a1"),
        content: vec![ContentBlock::ToolUse {
            id: ToolUseId::new("t1"),
            name: "Read".to_owned(),
            input: json!({"file_path": "/Users/dev/w/src/main.rs"}),
        }],
        model: "m".to_owned(),
        stop_reason: None,
        usage: None,
    }));
    history.push(Message::User(UserMessage {
        id: MessageId::new("r1"),
        content: vec![ContentBlock::ToolResult {
            tool_use_id: ToolUseId::new("t1"),
            content: vec![ToolResultContent::Text {
                text: "fn main() {}".to_owned(),
            }],
            is_error: false,
        }],
        origin: UserOrigin::ToolResult {
            assistant: MessageId::new("a1"),
        },
    }));
    let next = step(&context, &mut history).await;
    assert_eq!(sources(&next), ["nested_memory", "total_tokens_reminder"]);
    assert_eq!(next[0].role(), AttachmentRole::System);
    assert_eq!(
        next[0].text,
        "<system-reminder>\nContents of /Users/dev/w/src/AGENTS.md:\n\nsrc rules\n</system-reminder>"
    );
    // 同一批触发再跑一步（例如重试），已发过的不再发。
    assert_eq!(
        sources(&step(&context, &mut history).await),
        ["total_tokens_reminder"]
    );
}

/// Bash `cd` 之后，环境段的主工作目录跟着 shell 的当前目录；规则文件仍按
/// 项目根加载，不因此重新宣布。
#[tokio::test]
async fn a_shell_cd_announces_the_new_working_directory() {
    let env = Arc::new(environment());
    let context = assembler(env.clone());
    let mut history = vec![human()];
    step(&context, &mut history).await;
    history.push(Message::Assistant(
        rust_cli_kernel_api::message::AssistantMessage {
            id: MessageId::new("a1"),
            content: vec![ContentBlock::ToolUse {
                id: rust_cli_kernel_api::ids::ToolUseId::new("t1"),
                name: "Bash".to_owned(),
                input: serde_json::json!({"command": "cd sub"}),
            }],
            model: "m".to_owned(),
            stop_reason: None,
            usage: None,
        },
    ));
    history.push(Message::User(UserMessage {
        id: MessageId::new("r1"),
        content: vec![ContentBlock::ToolResult {
            tool_use_id: rust_cli_kernel_api::ids::ToolUseId::new("t1"),
            content: Vec::new(),
            is_error: false,
        }],
        origin: UserOrigin::ToolResult {
            assistant: MessageId::new("a1"),
        },
    }));
    env.set_shell_cwd("/Users/dev/w/sub");
    let next = step(&context, &mut history).await;
    assert_eq!(sources(&next), ["environment", "total_tokens_reminder"]);
    assert!(
        next[0]
            .text
            .contains("Primary working directory: /Users/dev/w/sub (was /Users/dev/w)"),
        "{}",
        next[0].text
    );
}
