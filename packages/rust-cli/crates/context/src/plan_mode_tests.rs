//! 计划模式 attachment 的节奏与文本（context.md §13）。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::env::{EnvFacts, EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{AgentId, EnvironmentId, MessageId, SessionId};
use rust_cli_kernel_api::listing::{ListingEntry, ListingSource, Listings};
use rust_cli_kernel_api::message::{AttachmentMessage, ContentBlock, UserMessage};
use rust_cli_kernel_api::model::{ModelSelection, QuerySource};
use rust_cli_kernel_api::orchestration::{ContextBuilder, ContextRequest, OrchestrationError};
use rust_cli_kernel_api::plan::PlanModeFlags;
use rust_cli_kernel_api::testing::{FixedClock, MemoryEnvironment, SequentialIds};
use serde_json::{Value, json};

use super::*;
use crate::{ContextConfig, ContextPorts, PromptAssembler, Surface};

const PLAN: &str = "/Users/dev/w/.zcode/plans/plan-sess_1.md";

pub(super) fn human() -> Message {
    Message::User(UserMessage {
        id: MessageId::new("u"),
        content: vec![ContentBlock::Text {
            text: "go".to_owned(),
        }],
        origin: UserOrigin::Human,
    })
}

pub(super) fn marker(source: &str) -> Message {
    Message::Attachment(AttachmentMessage {
        id: MessageId::new("p"),
        source: source.to_owned(),
        text: String::new(),
        data: Value::Null,
    })
}

pub(super) fn humans(count: usize) -> Vec<Message> {
    (0..count).map(|_| human()).collect()
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

fn assembler(home: Option<&str>, plan: Option<&str>) -> PromptAssembler {
    let facts = EnvFacts {
        cwd: EnvPath::new("/Users/dev/w"),
        platform: "linux".to_owned(),
        os_version: "Linux 6.8.0".to_owned(),
        shell: "bash".to_owned(),
        is_git_repo: false,
        additional_directories: Vec::new(),
        home: home.map(EnvPath::new),
    };
    let mut env = MemoryEnvironment::new(facts);
    if let Some(path) = plan {
        env = env.with_file(path, "# Plan\n");
    }
    let env = Arc::new(env);
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
        ContextConfig::new(Surface::Interactive),
    )
}

fn request(mode: PermissionMode, agent: Option<&str>) -> ContextRequest {
    let (session, source) = match agent {
        Some(_) => (
            "sess_subagent_agent_1",
            QuerySource::Subagent("general-purpose".to_owned()),
        ),
        None => ("sess_1", QuerySource::MainThread),
    };
    ContextRequest {
        capabilities: None,
        session: SessionId::new(session),
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/Users/dev/w"),
        },
        agent: agent.map(AgentId::new),
        source,
        mode,
        tool_names: ["Bash", "Skill"].map(str::to_owned).to_vec(),
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

async fn plan_attachments(
    assembler: &PromptAssembler,
    request: ContextRequest,
    history: &[Message],
) -> Vec<AttachmentMessage> {
    let built = assembler.step(request, history).await.unwrap();
    built
        .attachments
        .into_iter()
        .filter(|attachment| attachment.source.starts_with("plan_mode"))
        .collect()
}

fn sources(attachments: &[AttachmentMessage]) -> Vec<&str> {
    attachments
        .iter()
        .map(|attachment| attachment.source.as_str())
        .collect()
}

#[tokio::test]
async fn the_full_reminder_comes_after_the_skill_listing_and_names_the_plan_file() {
    let built = assembler(Some("/Users/dev"), None)
        .step(request(PermissionMode::Plan, None), &[human()])
        .await
        .unwrap();
    let order: Vec<&str> = built
        .attachments
        .iter()
        .map(|attachment| attachment.source.as_str())
        .collect();
    let at = |name: &str| order.iter().position(|source| *source == name).unwrap();
    assert!(at("skill_listing") < at(PLAN_MODE));
    assert!(at(PLAN_MODE) < at("total_tokens_reminder"));
    let reminder = &built.attachments[at(PLAN_MODE)];
    assert!(reminder.text.starts_with(
        "<system-reminder>\nPlan mode is active. The user indicated that they do not want you to execute yet"
    ));
    assert!(reminder.text.contains(&format!(
        "## Plan File Info:\nNo plan file exists yet. You should create your plan at {PLAN} using the Write tool.\nYou should build your plan"
    )));
    assert!(reminder.text.contains("### Phase 5: Call ExitPlanMode"));
    assert!(
        reminder
            .text
            .ends_with("before implementation begins.\n</system-reminder>")
    );
    assert_eq!(
        reminder.data,
        json!({"reminderType": "full", "isSubAgent": false, "planFilePath": PLAN, "planExists": false})
    );
}

#[tokio::test]
async fn an_existing_plan_file_is_offered_for_incremental_edits() {
    let found = plan_attachments(
        &assembler(Some("/Users/dev"), Some(PLAN)),
        request(PermissionMode::Plan, None),
        &[human()],
    )
    .await;
    assert_eq!(sources(&found), [PLAN_MODE]);
    assert!(found[0].text.contains(&format!(
        "A plan file already exists at {PLAN}. You can read it and make incremental edits using the Edit tool.\n"
    )));
    assert_eq!(found[0].data["planExists"], json!(true));
}

#[tokio::test]
async fn the_sparse_reminder_repeats_only_the_plan_file() {
    let mut history = vec![human(), marker(PLAN_MODE)];
    history.extend(humans(5));
    let found = plan_attachments(
        &assembler(Some("/Users/dev"), None),
        request(PermissionMode::Plan, None),
        &history,
    )
    .await;
    assert_eq!(
        found[0].text,
        format!(
            "<system-reminder>\nPlan mode still active (see full instructions earlier in conversation). Read-only except plan file ({PLAN}). Follow 5-phase workflow. End turns with AskUserQuestion (for clarifications) or ExitPlanMode (for plan approval). Never ask about plan approval via text or AskUserQuestion.\n</system-reminder>"
        )
    );
    assert_eq!(found[0].data["reminderType"], json!("sparse"));
}

fn with_flags(mut request: ContextRequest, flags: PlanModeFlags) -> ContextRequest {
    request.plan_flags = flags;
    request
}

#[tokio::test]
async fn reentering_with_a_plan_file_announces_the_reentry_first() {
    let history = vec![human(), marker(PLAN_MODE), marker(EXIT), human()];
    let exited = PlanModeFlags {
        has_exited: true,
        needs_exit_attachment: false,
    };
    let reentry = || with_flags(request(PermissionMode::Plan, None), exited);
    let found = plan_attachments(
        &assembler(Some("/Users/dev"), Some(PLAN)),
        reentry(),
        &history,
    )
    .await;
    assert_eq!(sources(&found), [REENTRY, PLAN_MODE]);
    assert!(found[0].text.starts_with(&format!(
        "<system-reminder>\n## Re-entering Plan Mode\n\nYou are returning to plan mode after having previously exited it. A plan file exists at {PLAN} from your previous planning session."
    )));
    assert_eq!(found[0].data, json!({"planFilePath": PLAN}));
    assert_eq!(found[1].data["reminderType"], json!("full"));
    // 计划文件不存在时没有 reentry。
    let found = plan_attachments(&assembler(Some("/Users/dev"), None), reentry(), &history).await;
    assert_eq!(sources(&found), [PLAN_MODE]);
    // 标志为假（resume 之后）时也没有 reentry，即使历史里有退出。
    let found = plan_attachments(
        &assembler(Some("/Users/dev"), Some(PLAN)),
        request(PermissionMode::Plan, None),
        &history,
    )
    .await;
    assert_eq!(sources(&found), [PLAN_MODE]);
}

#[tokio::test]
async fn the_exit_points_at_the_plan_file_only_when_it_exists() {
    let history = vec![human(), marker(PLAN_MODE)];
    let found = plan_attachments(
        &assembler(Some("/Users/dev"), Some(PLAN)),
        request(PermissionMode::AcceptEdits, None),
        &history,
    )
    .await;
    assert_eq!(
        found[0].text,
        format!(
            "<system-reminder>\n## Exited Plan Mode\n\nYou have exited plan mode. You can now make edits, run tools, and take actions. The plan file is located at {PLAN} if you need to reference it.\n</system-reminder>"
        )
    );
    assert_eq!(
        found[0].data,
        json!({"planFilePath": PLAN, "planExists": true})
    );
    let found = plan_attachments(
        &assembler(Some("/Users/dev"), None),
        request(PermissionMode::Default, None),
        &history,
    )
    .await;
    assert!(
        found[0]
            .text
            .ends_with("and take actions.\n</system-reminder>")
    );
}

#[tokio::test]
async fn subagents_get_their_own_text_and_plan_file() {
    // 子代理跑在子会话：同一目录下的 `plan-<子会话>.md`（tools-more.md §2.2、D15）。
    let plan = "/Users/dev/w/.zcode/plans/plan-sess_subagent_agent_1.md";
    // 子代理的 plan_mode 只在工具轮结束时的当步 attachment 里（F16，agents.md §4.3），这里直接取那一组。
    let found = assembler(Some("/Users/dev"), None)
        .turn_attachments(request(PermissionMode::Plan, Some("a1")), &[human()])
        .await
        .unwrap();
    let order: Vec<&str> = found
        .iter()
        .map(|attachment| attachment.source.as_str())
        .collect();
    let at = |name: &str| order.iter().position(|source| *source == name).unwrap();
    assert!(at("skill_listing") < at(PLAN_MODE));
    let reminder = &found[at(PLAN_MODE)];
    assert!(reminder.text.contains(&format!(
        "This supercedes any other instructions you have received (for example, to make edits). Instead, you should:\n\n## Plan File Info:\nNo plan file exists yet. You should create your plan at {plan} using the Write tool if you need to.\n"
    )));
    assert!(!reminder.text.contains("Phase 1"));
    assert_eq!(reminder.data["isSubAgent"], json!(true));
    assert_eq!(reminder.data["planFilePath"], json!(plan));
}

/// 计划文件在工作区里（Node 的位置），不再依赖 home。
#[tokio::test]
async fn the_plan_file_does_not_depend_on_the_home_directory() {
    let found = plan_attachments(
        &assembler(None, None),
        request(PermissionMode::Plan, None),
        &[human()],
    )
    .await;
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].data["planFilePath"], json!(PLAN));
}

/// F10（compact.md §3.4）：模式是 plan 时压缩附件带一条完整的 `plan_mode`，排在清单之前；
/// 不是 plan 时没有。以前由压缩后下一步的上下文补发，当步的 attachment 改到工具轮之后产生后放进压缩附件。
#[tokio::test]
async fn compaction_attaches_a_full_plan_reminder_in_plan_mode() {
    use rust_cli_kernel_api::compaction::PostCompactContext;
    let assembler = assembler(Some("/Users/dev"), Some(PLAN));
    let restored = assembler
        .post_compact(request(PermissionMode::Plan, None), &[])
        .await
        .unwrap();
    assert_eq!(
        restored.first().map(|item| item.source.as_str()),
        Some(PLAN_MODE)
    );
    assert_eq!(
        restored[0].data,
        json!({"reminderType": "full", "isSubAgent": false, "planFilePath": PLAN, "planExists": true})
    );
    let restored = assembler
        .post_compact(request(PermissionMode::Default, None), &[])
        .await
        .unwrap();
    assert!(
        restored
            .iter()
            .all(|item| !item.source.starts_with("plan_mode"))
    );
}
