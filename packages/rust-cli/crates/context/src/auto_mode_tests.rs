//! bypass 模式的宣布（shell-permissions.md §4.9、context.md §14，修复线 F43）。
//!
//! 文字与 data 是固定的；时点同当步 attachment：人类输入之后、每个工具轮结束，子代理只在工具轮结束时。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::env::{EnvFacts, EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{AgentId, EnvironmentId, MessageId, SessionId, ToolUseId};
use rust_cli_kernel_api::listing::{ListingEntry, ListingSource, Listings};
use rust_cli_kernel_api::message::{
    AssistantMessage, AttachmentMessage, CompactBoundary, CompactTrigger, ContentBlock, Message,
    SystemMessage, ToolResultContent, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::model::{ModelSelection, QuerySource};
use rust_cli_kernel_api::orchestration::{ContextBuilder, ContextRequest, OrchestrationError};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::{FixedClock, MemoryEnvironment, SequentialIds};
use serde_json::{Value, json};

use super::*;
use crate::{ContextConfig, ContextPorts, PromptAssembler, Surface};

/// bypassPermissions 下的文字（`bashFirstSteer: "relaxed"`）。
const STEER: &str = "While bypass permissions mode is active:\n\nYou can do much of your work through the Bash tool when it is the simpler route: read files with cat, head, or sed -n, search with grep and find, and make small, mechanical file changes with sed, heredocs, or short scripts instead of the dedicated Read, Edit, or Write tools. The choice is yours: prefer Edit or Write when a shell edit would be fragile, such as exact or multi-line replacements, or sed/awk flags that differ between GNU and BSD/macOS.";

/// CL4 / BP1 的工具清单里与条件有关的几个。
const TOOLS: &[&str] = &["Agent", "Bash", "Edit", "Read", "Skill", "Write"];

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| (*name).to_owned()).collect()
}

fn human() -> Message {
    Message::User(UserMessage {
        id: MessageId::new("u"),
        content: vec![ContentBlock::Text {
            text: "go".to_owned(),
        }],
        origin: UserOrigin::Human,
    })
}

fn attachment(source: &str) -> Message {
    Message::Attachment(AttachmentMessage {
        id: MessageId::new("m"),
        source: source.to_owned(),
        text: String::new(),
        data: Value::Null,
    })
}

/// 一个工具轮：assistant 的 Bash 调用与它的结果。
fn tool_round() -> Vec<Message> {
    vec![
        Message::Assistant(AssistantMessage {
            id: MessageId::new("a"),
            content: vec![ContentBlock::ToolUse {
                id: ToolUseId::new("t"),
                name: "Bash".to_owned(),
                input: json!({"command": "echo hi"}),
            }],
            model: rust_cli_base::models::GLM_5_3.to_owned(),
            stop_reason: None,
            usage: None,
        }),
        Message::User(UserMessage {
            id: MessageId::new("r"),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: ToolUseId::new("t"),
                content: vec![ToolResultContent::Text {
                    text: "hi".to_owned(),
                }],
                is_error: false,
            }],
            origin: UserOrigin::ToolResult {
                assistant: MessageId::new("a"),
            },
        }),
    ]
}

fn boundary() -> Message {
    Message::System(SystemMessage::CompactBoundary(CompactBoundary {
        id: MessageId::new("b"),
        trigger: CompactTrigger::Reactive,
        pre_tokens: 1,
        summary: MessageId::new("s"),
        preserved: None,
        discovered_tools: Vec::new(),
    }))
}

#[test]
fn the_bypass_category_announces_bash_first_with_the_relaxed_steer() {
    for mode in [PermissionMode::BypassPermissions, PermissionMode::Guarded] {
        let (text, data) = announce(&[human()], mode, &names(TOOLS)).unwrap();
        assert_eq!(text, STEER);
        // 记录的字段：bypass 时没有 consent flow，steer 是 relaxed。
        assert_eq!(
            data,
            json!({
                "autoModeConsentFlow": false,
                "bashFirst": true,
                "bashFirstSteer": "relaxed",
                "steerOnly": true,
                "bypass": true,
            })
        );
    }
}

/// 模式不是 bypassPermissions（也不是 auto）时不产生；plan 开着时判决用的模式是 plan。
#[test]
fn other_modes_do_not_announce() {
    for mode in [
        PermissionMode::Default,
        PermissionMode::AcceptEdits,
        PermissionMode::Plan,
        PermissionMode::DontAsk,
        PermissionMode::Auto,
    ] {
        assert_eq!(announce(&[human()], mode, &names(TOOLS)), None, "{mode:?}");
    }
}

/// bashFirst：有 Bash，且有 Edit 或 Write；bypass（steerOnly）而不是 bashFirst 时不产生。
#[test]
fn bash_first_needs_bash_and_a_file_writing_tool() {
    let bypass = PermissionMode::BypassPermissions;
    let check = |list: &[&str]| announce(&[human()], bypass, &names(list)).is_some();
    assert!(!check(&["Bash", "Read"]));
    assert!(!check(&["Edit", "Read", "Write"]));
    assert!(check(&["Bash", "Edit"]));
    assert!(check(&["Bash", "Write"]));
}

/// 最近一条是 `auto_mode` 时不再宣布；最近一条是 `auto_mode_exit` 时可以再宣布。
#[test]
fn the_latest_announcement_decides_whether_to_repeat() {
    let bypass = PermissionMode::BypassPermissions;
    let tools = names(TOOLS);
    let announced = [human(), attachment(SOURCE), human()];
    assert_eq!(announce(&announced, bypass, &tools), None);
    let exited = [attachment(SOURCE), attachment("auto_mode_exit"), human()];
    assert!(announce(&exited, bypass, &tools).is_some());
    let again = [attachment("auto_mode_exit"), attachment(SOURCE), human()];
    assert_eq!(announce(&again, bypass, &tools), None);
}

struct Skills;

#[async_trait]
impl ListingSource for Skills {
    async fn listings(&self, _request: &ContextRequest) -> Result<Listings, OrchestrationError> {
        Ok(Listings {
            skills: vec![ListingEntry {
                name: "review".to_owned(),
                line: "- review".to_owned(),
            }],
            ..Listings::default()
        })
    }
}

fn assembler() -> PromptAssembler {
    let facts = EnvFacts {
        cwd: EnvPath::new("/Users/dev/w"),
        platform: "darwin".to_owned(),
        os_version: "Darwin 24.6.0".to_owned(),
        shell: "bash".to_owned(),
        is_git_repo: false,
        additional_directories: Vec::new(),
        home: None,
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
            listings: Arc::new(Skills),
        },
        ContextConfig::new(Surface::Print),
    )
}

fn request(mode: PermissionMode, subagent: bool) -> ContextRequest {
    let (session, source, agent) = if subagent {
        (
            "sess_subagent_agent_1",
            QuerySource::Subagent("general-purpose".to_owned()),
            Some(AgentId::new("agent_1")),
        )
    } else {
        ("sess_1", QuerySource::MainThread, None)
    };
    ContextRequest {
        default_machine: None,
        capabilities: None,
        session: SessionId::new(session),
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/Users/dev/w"),
        },
        agent,
        source,
        mode,
        tool_names: names(TOOLS),
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

fn sources(attachments: &[AttachmentMessage]) -> Vec<&str> {
    attachments
        .iter()
        .map(|attachment| attachment.source.as_str())
        .collect()
}

/// 第一个主请求里排在 skill 清单之后、`<total_tokens>` 之前（当步 attachment 的生产者顺序），
/// 角色 system，落库的文字带 `<system-reminder>` 包装。
#[tokio::test]
async fn the_first_step_announces_after_the_skill_listing_and_before_total_tokens() {
    let assembler = assembler();
    let mode = PermissionMode::BypassPermissions;
    let built = assembler
        .step(request(mode, false), &[human()])
        .await
        .unwrap();
    let order = sources(&built.attachments);
    let at = order.iter().position(|source| *source == SOURCE).unwrap();
    assert_eq!(order[at - 1], "skill_listing", "{order:?}");
    assert_eq!(order[at + 1], "total_tokens_reminder", "{order:?}");
    let announced = &built.attachments[at];
    assert_eq!(announced.text, rust_cli_prompts::reminder::wrap(STEER));
    assert_eq!(
        announced.role(),
        rust_cli_kernel_api::message::AttachmentRole::System
    );
    // 默认模式没有这一条。
    let plain = assembler
        .step(request(PermissionMode::Default, false), &[human()])
        .await
        .unwrap();
    assert!(!sources(&plain.attachments).contains(&SOURCE));
}

/// 回合中途切到 yolo：下一个工具轮结束时宣布一次；之后的工具轮不再宣布。
#[tokio::test]
async fn switching_into_bypass_mid_turn_announces_at_the_next_tool_round_once() {
    let assembler = assembler();
    let mut history = vec![human()];
    history.extend(tool_round());
    let default = assembler
        .turn_attachments(request(PermissionMode::Default, false), &history)
        .await
        .unwrap();
    assert!(!sources(&default).contains(&SOURCE));
    let bypass = PermissionMode::BypassPermissions;
    let first = assembler
        .turn_attachments(request(bypass, false), &history)
        .await
        .unwrap();
    assert!(sources(&first).contains(&SOURCE));
    history.extend(first.into_iter().map(Message::Attachment));
    history.extend(tool_round());
    let second = assembler
        .turn_attachments(request(bypass, false), &history)
        .await
        .unwrap();
    assert!(!sources(&second).contains(&SOURCE));
}

/// 压缩边界之后窗口里没有宣布：下一个时点再宣布一次（压缩附件里没有 `auto_mode`）。
#[tokio::test]
async fn a_compaction_boundary_brings_the_announcement_back() {
    let assembler = assembler();
    let history = vec![human(), attachment(SOURCE), boundary(), human()];
    let after = assembler
        .turn_attachments(request(PermissionMode::BypassPermissions, false), &history)
        .await
        .unwrap();
    assert!(sources(&after).contains(&SOURCE));
}

/// 子代理开头那一组没有它，第一个工具轮结束时宣布。
#[tokio::test]
async fn subagents_announce_at_their_first_tool_round() {
    let assembler = assembler();
    let bypass = PermissionMode::BypassPermissions;
    let start = assembler
        .start_attachments(request(bypass, true), &[human()])
        .await
        .unwrap();
    assert!(!sources(&start).contains(&SOURCE));
    let mut history = vec![human()];
    history.extend(start.into_iter().map(Message::Attachment));
    history.extend(tool_round());
    let round = assembler
        .turn_attachments(request(bypass, true), &history)
        .await
        .unwrap();
    let order = sources(&round);
    let at = order.iter().position(|source| *source == SOURCE).unwrap();
    assert_eq!(order[at + 1], "total_tokens_reminder", "{order:?}");
}

/// plan 开着（F20）：判决用的模式是 plan，不宣布；关掉 plan 回到 yolo 后的下一个时点宣布。
#[tokio::test]
async fn plan_over_bypass_waits_until_plan_is_off() {
    let assembler = assembler();
    let mut history = vec![human()];
    let planned = assembler
        .turn_attachments(request(PermissionMode::Plan, false), &history)
        .await
        .unwrap();
    assert!(!sources(&planned).contains(&SOURCE));
    history.extend(planned.into_iter().map(Message::Attachment));
    history.extend(tool_round());
    let after = assembler
        .turn_attachments(request(PermissionMode::BypassPermissions, false), &history)
        .await
        .unwrap();
    assert!(sources(&after).contains(&SOURCE));
}
