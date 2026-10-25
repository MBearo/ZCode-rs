use std::sync::Arc;

use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, SessionId};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::{FixedClock, SequentialIds, UnusedEnvironment};

use super::*;

fn request(source: QuerySource) -> ContextRequest {
    ContextRequest {
        default_machine: None,
        capabilities: None,
        session: SessionId::new("s"),
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/w"),
        },
        agent: None,
        source,
        mode: PermissionMode::Default,
        tool_names: Vec::new(),
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

fn assembler() -> PromptAssembler {
    let env = Arc::new(UnusedEnvironment);
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

/// 回合从一条人类输入开始。
fn prompt() -> Vec<Message> {
    vec![Message::User(rust_cli_kernel_api::message::UserMessage {
        id: rust_cli_kernel_api::ids::MessageId::new("msg_u"),
        content: vec![rust_cli_kernel_api::message::ContentBlock::Text {
            text: "hi".to_owned(),
        }],
        origin: rust_cli_kernel_api::message::UserOrigin::Human,
    })]
}

#[tokio::test]
async fn the_main_thread_gets_its_announcements_without_environment_facts() {
    let built = assembler()
        .step(request(QuerySource::MainThread), &prompt())
        .await
        .unwrap();
    assert_eq!(built.system.len(), 3);
    let sources: Vec<&str> = built
        .attachments
        .iter()
        .map(|item| item.source.as_str())
        .collect();
    // 没有 Bash 工具时不宣布署名。
    assert_eq!(sources, ["model", "total_tokens_reminder", "date"]);
    assert!(
        built
            .attachments
            .iter()
            .all(|item| item.text.starts_with("<system-reminder>\n"))
    );
    assert_eq!(
        built.attachments[2].text,
        "<system-reminder>\nToday's date is 2026-10-17.\n</system-reminder>"
    );
}

#[tokio::test]
async fn the_attribution_needs_the_bash_tool() {
    let mut with_bash = request(QuerySource::MainThread);
    with_bash.tool_names = vec!["Bash".to_owned()];
    let built = assembler().build(with_bash, &[]).await.unwrap();
    assert_eq!(
        built.attachments.last().map(|item| item.source.as_str()),
        Some("remote_session_change")
    );
}

#[tokio::test]
async fn other_query_sources_get_no_announcements() {
    let built = assembler()
        .build(request(QuerySource::Side("title".to_owned())), &[])
        .await
        .unwrap();
    assert!(built.attachments.is_empty());
    assert_eq!(built.system.len(), 3);
    // 摘要请求只补消息里没有的宣布（context.md §4.1）：这里没有执行环境事实，只有日期。
    let built = assembler()
        .build(request(QuerySource::Compact), &[])
        .await
        .unwrap();
    let sources: Vec<&str> = built
        .attachments
        .iter()
        .map(|item| item.source.as_str())
        .collect();
    assert_eq!(sources, ["date"]);
}

/// F10（query.md §1.1）：主线程的一步拆成两个时点。当步的 attachment（模型、剩余 token…）
/// 只在 `turn_attachments` 里，`build` 只给上下文宣布（日期、署名）；摘要与旁路请求没有当步的
/// attachment（子代理见 `subagent_tests.rs`，F16）。旧代码在 `build` 里一次给出全部，`turn_attachments` 是缺省的空表。
#[tokio::test]
async fn the_main_thread_step_splits_turn_attachments_from_announcements() {
    let assembler = assembler();
    let turn = assembler
        .turn_attachments(request(QuerySource::MainThread), &prompt())
        .await
        .unwrap();
    let turn: Vec<&str> = turn.iter().map(|item| item.source.as_str()).collect();
    assert_eq!(turn, ["model", "total_tokens_reminder"]);
    let built = assembler
        .build(request(QuerySource::MainThread), &prompt())
        .await
        .unwrap();
    let announced: Vec<&str> = built
        .attachments
        .iter()
        .map(|item| item.source.as_str())
        .collect();
    assert_eq!(announced, ["date"]);
    for source in [QuerySource::Compact, QuerySource::Side("title".to_owned())] {
        let turn = assembler
            .turn_attachments(request(source), &prompt())
            .await
            .unwrap();
        assert!(turn.is_empty());
    }
}

/// 剩余 token 跨压缩（工作线 F82）：压缩前这次输入里用掉的不因压缩归还，压缩之后的工具轮照样从输入那一刻算。
/// 以前只看边界之后的历史，压缩之后回到满额。
#[tokio::test]
async fn the_tokens_left_do_not_reset_at_a_compaction() {
    use rust_cli_kernel_api::ids::{MessageId, ToolUseId};
    use rust_cli_kernel_api::message::{
        AssistantMessage, CompactBoundary, CompactTrigger, ContentBlock, ToolResultContent, Usage,
        UserMessage, UserOrigin,
    };
    let round = |index: &str, input_tokens: u64| {
        let assistant = MessageId::new(format!("a{index}"));
        [
            Message::Assistant(AssistantMessage {
                id: assistant.clone(),
                content: vec![ContentBlock::ToolUse {
                    id: ToolUseId::new(format!("t{index}")),
                    name: "Read".to_owned(),
                    input: serde_json::json!({}),
                }],
                model: "m".to_owned(),
                stop_reason: None,
                usage: Some(Usage {
                    input_tokens,
                    output_tokens: 16,
                    ..Usage::default()
                }),
            }),
            Message::User(UserMessage {
                id: MessageId::new(format!("r{index}")),
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: ToolUseId::new(format!("t{index}")),
                    content: vec![ToolResultContent::Text {
                        text: "ok".to_owned(),
                    }],
                    is_error: false,
                }],
                origin: UserOrigin::ToolResult { assistant },
            }),
        ]
    };
    let mut history = prompt();
    history.extend(round("1", 990_000));
    history.push(Message::System(SystemMessage::CompactBoundary(
        CompactBoundary {
            id: MessageId::new("b"),
            trigger: CompactTrigger::Auto,
            pre_tokens: 990_016,
            summary: MessageId::new("s"),
            preserved: None,
            discovered_tools: Vec::new(),
        },
    )));
    history.push(Message::User(UserMessage {
        id: MessageId::new("s"),
        content: vec![ContentBlock::Text {
            text: "summary".to_owned(),
        }],
        origin: UserOrigin::CompactSummary,
    }));
    history.extend(round("2", 10));
    let turn = assembler()
        .turn_attachments(request(QuerySource::MainThread), &history)
        .await
        .unwrap();
    let tokens = turn
        .iter()
        .find(|item| item.source == "total_tokens_reminder")
        .unwrap();
    // 压缩前的 990,016（990,000 + 16）滚进累计，加上压缩之后这一轮的 10 + 16。
    assert!(
        tokens.text.contains("14009958 tokens left"),
        "{}",
        tokens.text
    );
}
