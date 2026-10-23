//! 子代理的 attachment：三个时点、顺序与省略（docs/specs/rust-cli/agents.md §4.3，修复线 F16）：第一个请求与
//! 工具轮之后的请求。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::env::{EnvFacts, EnvPath, WorkspaceRef};
use rust_cli_kernel_api::env_git::{GitSnapshot, GitStatus};
use rust_cli_kernel_api::ids::{AgentId, EnvironmentId, MessageId, SessionId, ToolUseId};
use rust_cli_kernel_api::listing::{ListingEntry, ListingSource, Listings};
use rust_cli_kernel_api::message::{
    AssistantMessage, AttachmentMessage, ContentBlock, Message, ToolResultContent, Usage,
    UserMessage, UserOrigin,
};
use rust_cli_kernel_api::model::{ModelSelection, QuerySource};
use rust_cli_kernel_api::orchestration::{ContextBuilder, ContextRequest, OrchestrationError};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::{FixedClock, MemoryEnvironment, SequentialIds};

use crate::{ContextConfig, ContextPorts, PromptAssembler, Surface};

struct Fixed;

#[async_trait]
impl ListingSource for Fixed {
    async fn listings(&self, _request: &ContextRequest) -> Result<Listings, OrchestrationError> {
        let entry = |name: &str| ListingEntry {
            name: name.to_owned(),
            line: format!("- {name}"),
        };
        Ok(Listings {
            deferred_tools: vec![entry("WebFetch")],
            agents: vec![entry("Explore")],
            skills: vec![entry("review")],
        })
    }
}

fn assembler() -> PromptAssembler {
    assembler_in(None)
}

/// `shell_cwd`：子会话 shell 的当前目录（agents 派生时设成父会话当时的目录，tools-files.md §3.3）。
fn assembler_in(shell_cwd: Option<&str>) -> PromptAssembler {
    let facts = EnvFacts {
        cwd: EnvPath::new("/Users/dev/w"),
        platform: "linux".to_owned(),
        os_version: "Linux 6.8.0".to_owned(),
        shell: "bash".to_owned(),
        is_git_repo: true,
        additional_directories: Vec::new(),
        home: Some(EnvPath::new("/Users/dev")),
    };
    let env = MemoryEnvironment::new(facts)
        .with_file("/Users/dev/w/AGENTS.md", "Keep answers short.")
        .with_git(GitStatus::Repository(GitSnapshot {
            branch: "main".to_owned(),
            default_branch: "main".to_owned(),
            status: String::new(),
            recent_commits: "090e5cc Add the agent fixtures".to_owned(),
            user_name: Some("Dev Example".to_owned()),
        }));
    if let Some(cwd) = shell_cwd {
        env.set_shell_cwd(cwd);
    }
    PromptAssembler::new(
        ContextPorts {
            env: Arc::new(env),
            clock: Arc::new(FixedClock(1_792_224_000_000)),
            ids: Arc::new(SequentialIds::default()),
            listings: Arc::new(Fixed),
        },
        ContextConfig::new(Surface::Print),
    )
}

fn request(agent_type: &str) -> ContextRequest {
    ContextRequest {
        capabilities: None,
        session: SessionId::new("sess_subagent_agent_1"),
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/Users/dev/w"),
        },
        agent: Some(AgentId::new("agent_1")),
        source: QuerySource::Subagent(agent_type.to_owned()),
        mode: PermissionMode::Default,
        tool_names: ["Agent", "Bash", "Skill"].map(str::to_owned).to_vec(),
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

fn sources(attachments: &[AttachmentMessage]) -> Vec<&str> {
    attachments
        .iter()
        .map(|attachment| attachment.source.as_str())
        .collect()
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

/// 一个工具轮：调 Bash 的 assistant（这次响应占用 `tokens` 个上下文 token）与它的结果。
fn round(n: usize, tokens: u64) -> [Message; 2] {
    let assistant = MessageId::new(format!("a{n}"));
    let call = ToolUseId::new(format!("toolu_{n}"));
    [
        Message::Assistant(AssistantMessage {
            id: assistant.clone(),
            content: vec![ContentBlock::ToolUse {
                id: call.clone(),
                name: "Bash".to_owned(),
                input: serde_json::json!({"command": "true"}),
            }],
            model: "claude-opus-5-5".to_owned(),
            stop_reason: None,
            usage: Some(Usage {
                input_tokens: tokens,
                ..Usage::default()
            }),
        }),
        Message::User(UserMessage {
            id: MessageId::new(format!("r{n}")),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: call,
                content: vec![ToolResultContent::Text {
                    text: "ok".to_owned(),
                }],
                is_error: false,
            }],
            origin: UserOrigin::ToolResult { assistant },
        }),
    ]
}

/// 子循环的运行开始：开头的一组落库，再是第一步的上下文宣布（query.md §1.1）。返回两组与之后的历史。
async fn start(
    assembler: &PromptAssembler,
    request: &ContextRequest,
    mut history: Vec<Message>,
) -> (Vec<AttachmentMessage>, Vec<AttachmentMessage>, Vec<Message>) {
    let opening = assembler
        .start_attachments(request.clone(), &history)
        .await
        .unwrap();
    history.extend(opening.iter().cloned().map(Message::Attachment));
    let built = assembler.build(request.clone(), &history).await.unwrap();
    history.extend(built.attachments.iter().cloned().map(Message::Attachment));
    (opening, built.attachments, history)
}

/// 进入子循环前产生延迟工具、环境、模型、skill，子循环
/// 的第一步再宣布规则文件、会话上下文、日期、署名。没有 agent 清单与剩余 token。
/// 旧代码运行开始不产生任何东西，全部在 `build` 里。
#[tokio::test]
async fn general_purpose_starts_with_the_runsubagent_group_then_the_announcements() {
    let assembler = assembler();
    let request = request("general-purpose");
    let (opening, announced, _) = start(&assembler, &request, vec![user("p", "Go.")]).await;
    assert_eq!(
        sources(&opening),
        [
            "deferred_tools_delta",
            "environment",
            "model",
            "skill_listing"
        ]
    );
    assert_eq!(
        sources(&announced),
        [
            "instructions",
            "session_context",
            "date",
            "remote_session_change"
        ]
    );
}

#[tokio::test]
async fn explore_and_plan_omit_the_rule_files_and_the_git_status() {
    for agent_type in ["Explore", "Plan"] {
        let assembler = assembler();
        let request = request(agent_type);
        let (opening, announced, _) = start(&assembler, &request, vec![user("p", "Go.")]).await;
        assert_eq!(
            sources(&opening),
            [
                "deferred_tools_delta",
                "environment",
                "model",
                "skill_listing"
            ]
        );
        assert_eq!(sources(&announced), ["date", "remote_session_change"]);
    }
}

/// 每个工具轮结束时产生当步的 attachment（agentId 是子代理）：第一次
/// 宣布 agent 清单，每次附剩余 token；没有主线程独有的静默提醒。之后的上下文宣布没有新内容。
/// 旧代码工具轮结束时什么也不产生，`build` 里也不发 agent 清单与剩余 token。
#[tokio::test]
async fn each_tool_round_announces_the_agent_listing_once_and_the_tokens_left() {
    let assembler = assembler();
    let request = request("general-purpose");
    let (_, _, mut history) = start(&assembler, &request, vec![user("p", "Go.")]).await;
    history.extend(round(1, 13));
    let first = assembler
        .turn_attachments(request.clone(), &history)
        .await
        .unwrap();
    assert_eq!(
        sources(&first),
        ["agent_listing_delta", "total_tokens_reminder"]
    );
    assert!(
        first[1]
            .text
            .contains("<total_tokens>14999987 tokens left</total_tokens>")
    );
    history.extend(first.into_iter().map(Message::Attachment));
    let built = assembler.build(request.clone(), &history).await.unwrap();
    assert!(built.attachments.is_empty());
    history.extend(round(2, 20));
    let second = assembler
        .turn_attachments(request.clone(), &history)
        .await
        .unwrap();
    assert_eq!(sources(&second), ["total_tokens_reminder"]);
}

/// 剩余 token 不重新锚定（只在主线程的人类输入那一步重新锚定）：SendMessage 续跑的消息之后，
/// 已用量仍是这个子代理历次响应的最大值。主线程的算法会从续跑消息重新计数（130 − 120 = 10）。
#[tokio::test]
async fn a_resumed_subagent_keeps_counting_the_tokens_left_from_its_start() {
    let assembler = assembler();
    let request = request("general-purpose");
    let mut history = vec![user("p", "Go.")];
    history.extend(round(1, 100));
    history.push(Message::Assistant(AssistantMessage {
        id: MessageId::new("a_done"),
        content: vec![ContentBlock::Text {
            text: "Done.".to_owned(),
        }],
        model: "claude-opus-5-5".to_owned(),
        stop_reason: None,
        usage: Some(Usage {
            input_tokens: 120,
            ..Usage::default()
        }),
    }));
    history.push(user("resume", "Also check the logs."));
    history.extend(round(2, 130));
    let found = assembler.turn_attachments(request, &history).await.unwrap();
    let tokens = found
        .iter()
        .find(|attachment| attachment.source == "total_tokens_reminder")
        .unwrap();
    assert!(
        tokens
            .text
            .contains("<total_tokens>14999870 tokens left</total_tokens>")
    );
}

/// 随父会话进入 plan 的子代理：第一个请求没有 `plan_mode`（运行开始的那一组不产生它），第一个工具轮结束时
/// 才有，排在剩余 token 之前（当步 attachment 里 `plan_mode` 在 `total_tokens_reminder` 之前）。旧代码在第一个
/// 请求里就给出。
#[tokio::test]
async fn a_plan_mode_subagent_gets_the_plan_reminder_after_its_first_tool_round() {
    let assembler = assembler();
    let mut request = request("general-purpose");
    request.mode = PermissionMode::Plan;
    let (opening, announced, mut history) =
        start(&assembler, &request, vec![user("p", "Go.")]).await;
    assert!(!sources(&opening).contains(&"plan_mode"));
    assert!(!sources(&announced).contains(&"plan_mode"));
    history.extend(round(1, 13));
    let found = assembler.turn_attachments(request, &history).await.unwrap();
    assert_eq!(
        sources(&found),
        ["agent_listing_delta", "plan_mode", "total_tokens_reminder"]
    );
    assert_eq!(found[1].data["isSubAgent"], serde_json::json!(true));
}

/// F19：父会话 `cd sub` 之后派的子代理，运行开始的环境段就写子会话的当前目录（子代理的当前目录是派生时复制的
/// 目录），工具轮不再补 `# Environment update`。修前运行开始按项目根、工具轮按子会话的 shell cwd，两处不一致。
#[tokio::test]
async fn the_environment_shows_the_directory_the_subagent_inherited() {
    let assembler = assembler_in(Some("/Users/dev/w/sub"));
    let request = request("general-purpose");
    let (opening, _, mut history) = start(&assembler, &request, vec![user("p", "Go.")]).await;
    let environment = opening
        .iter()
        .find(|attachment| attachment.source == "environment")
        .unwrap();
    assert_eq!(
        environment.data["snapshot"]["workingDirectory"],
        "/Users/dev/w/sub"
    );
    history.extend(round(1, 10_000));
    let found = assembler.turn_attachments(request, &history).await.unwrap();
    assert!(
        !sources(&found).contains(&"environment"),
        "{:?}",
        sources(&found)
    );
}
