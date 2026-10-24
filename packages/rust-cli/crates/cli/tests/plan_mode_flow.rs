//! 计划模式端到端（docs/specs/rust-cli/tools-more.md §2、§8；context.md §13）：真实的 LocalKernel（TUI
//! 形态）、真实 env-local（临时 home 与工作区）、脚本模型与计划审批的回答。
//!
//! 剧本：EnterPlanMode → Write 计划文件（plan 模式下放行）→ Write 工作区文件（plan 模式下被拦）→
//! ExitPlanMode（批准）→ 结束语。逐个请求检查计划模式 attachment 的出现与节奏。
//!
//! 修复线 F20：会话从 guarded 开始；plan 是独立开关，plan 下 guarded 也照 plan 拦写入；批准只关掉 plan（回答里的
//! acceptEdits 不用），之后回到进入前的 guarded。

#![allow(
    clippy::unwrap_used,
    clippy::disallowed_methods,
    reason = "集成测试：直接准备临时 home 与工作区，并检查写入的结果"
)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use rust_cli_env_local::LocalEnvironment;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{CommandId, EnvironmentId, SessionId};
use rust_cli_kernel_api::message::{ContentBlock, ToolResultContent};
use rust_cli_kernel_api::model::ModelRequest;
use rust_cli_kernel_api::orchestration::{InteractionRequest, PlanApprovalAnswer};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{
    Command, CommandEnvelope, CreateSession, Delivery, Kernel, KernelEvent, TurnOutcome,
};
use rust_cli_kernel_api::testing::{
    FixedClock, MemoryStore, RoundTripEnv, ScriptedModel, SequentialIds, environments, selection,
    text_response, tool_use_response,
};
use rust_cli_runtime::{Defaults, LocalKernel, Ports};
use serde_json::json;

const SESSION: &str = "sess_plan_flow";

/// 一个请求里模型看到的全部文本（system 段之外）：文本块与工具结果，按出现顺序拼接。
fn visible(request: &ModelRequest) -> String {
    let mut out = String::new();
    for content in request
        .messages
        .iter()
        .flat_map(|message| message.content.iter())
    {
        match &content.block {
            ContentBlock::Text { text } => out.push_str(text),
            ContentBlock::ToolResult { content, .. } => {
                for part in content {
                    if let ToolResultContent::Text { text } = part {
                        out.push_str(text);
                    }
                }
            }
            _ => {}
        }
        out.push('\n');
    }
    out
}

fn count(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

async fn run(dir: &Path) -> (Arc<ScriptedModel>, String) {
    let work = dir.join("work");
    let home = dir.join("home");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    // Node 的位置：`<工作区>/.zcode/plans/plan-<会话>.md`（tools-more.md §2.2）。
    let plan = work
        .join(".zcode/plans")
        .join(format!("plan-{SESSION}.md"))
        .to_string_lossy()
        .into_owned();
    let blocked = work.join("src.txt").to_string_lossy().into_owned();
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "EnterPlanMode", &json!({})),
        tool_use_response(
            "toolu_2",
            "Write",
            &json!({"file_path": plan, "content": "# Plan\n\nAdd the feature.\n"}),
        ),
        tool_use_response(
            "toolu_3",
            "Write",
            &json!({"file_path": blocked, "content": "too early\n"}),
        ),
        tool_use_response("toolu_4", "ExitPlanMode", &json!({})),
        text_response(&["Plan approved; starting."]),
    ]));
    let store = Arc::new(MemoryStore::default());
    let env = LocalEnvironment::new(Some(home));
    let kernel = LocalKernel::new(
        Ports {
            model: model.clone(),
            environments: environments(Arc::new(RoundTripEnv(Arc::new(env)))),
            store: store.clone(),
            artifacts: store,
            clock: Arc::new(FixedClock(1_000)),
            ids: Arc::new(SequentialIds::default()),
        },
        Defaults {
            selection: selection(),
            mode: PermissionMode::Default,
            max_turns: None,
            search_tools: false,
            task_tools: None,
            limits: Default::default(),
        },
    );
    let id = kernel
        .create_session(CreateSession {
            id: Some(SessionId::new(SESSION)),
            workspace: WorkspaceRef {
                environment: EnvironmentId::new("local"),
                root: EnvPath::new(work.to_string_lossy()),
            },
            selection: selection(),
            mode: PermissionMode::Guarded,
            plan: false,
        })
        .await
        .unwrap();
    let session = kernel.open_session(&id).await.unwrap();
    let mut events = session.subscribe(Some(0));
    session
        .send(CommandEnvelope {
            id: CommandId::new("c1"),
            command: Command::Submit {
                text: "Plan the feature, then start.".to_owned(),
                attachments: Vec::new(),
                delivery: Delivery::Queue,
            },
        })
        .await;
    let mut modes = Vec::new();
    let mut plans = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.next())
            .await
            .unwrap()
            .unwrap();
        match event.event {
            KernelEvent::InteractionAsked {
                interaction,
                request: InteractionRequest::PlanApproval { plan, .. },
            } => {
                assert_eq!(plan, "# Plan\n\nAdd the feature.\n");
                let answer = PlanApprovalAnswer::Approve {
                    mode: PermissionMode::AcceptEdits,
                };
                session
                    .send(CommandEnvelope {
                        id: CommandId::new("c2"),
                        command: Command::ResolveInteraction {
                            interaction,
                            answer: serde_json::to_value(answer).unwrap(),
                        },
                    })
                    .await;
            }
            KernelEvent::ModeChanged { mode } => modes.push(mode),
            KernelEvent::PlanChanged { enabled } => plans.push(enabled),
            KernelEvent::TurnFinished { outcome, .. } => {
                assert_eq!(outcome, TurnOutcome::Success);
                break;
            }
            _ => {}
        }
    }
    // 只有 plan 开关变了：进入、批准后退出；权限模式没有变过，退出后还是进入前的 guarded。
    assert!(modes.is_empty(), "{modes:?}");
    assert_eq!(plans, [true, false]);
    let view = session.view().await.unwrap();
    assert_eq!((view.mode, view.plan), (PermissionMode::Guarded, false));
    assert_eq!(
        std::fs::read_to_string(&plan).unwrap(),
        "# Plan\n\nAdd the feature.\n"
    );
    assert!(!Path::new(&blocked).exists());
    drop(kernel);
    (model, plan)
}

#[tokio::test]
async fn enter_write_the_plan_and_exit() {
    let dir = tempfile::tempdir().unwrap();
    let (model, plan) = run(dir.path()).await;
    let requests: Vec<String> = model.requests().iter().map(visible).collect();
    assert_eq!(requests.len(), 5);
    let full = "Plan mode is active. The user indicated that they do not want you to execute yet";
    let exit = "## Exited Plan Mode";
    // 1：default 模式，没有计划模式提醒。
    assert_eq!(count(&requests[0], full), 0);
    // 2：EnterPlanMode 之后的那一步就有完整提醒，计划文件还不存在。
    assert!(requests[1].contains("Entered plan mode"), "{}", requests[1]);
    assert_eq!(count(&requests[1], full), 1);
    assert!(requests[1].contains(&format!(
        "No plan file exists yet. You should create your plan at {plan} using the Write tool."
    )));
    // 3：写计划文件放行；同一回合不重发提醒。
    assert!(requests[2].contains(&format!("File created successfully at: {plan}")));
    assert_eq!(count(&requests[2], full), 1);
    // 4：plan 模式下写工作区文件被拦（分类器不可用）。
    assert!(
        requests[3].contains("Auto mode could not evaluate this action"),
        "{}",
        requests[3]
    );
    // 5：批准后切到 acceptEdits；下一步发一次退出提醒，指向计划文件。
    assert!(requests[4].contains("User has approved your plan."));
    assert_eq!(count(&requests[4], exit), 1);
    assert!(requests[4].contains(&format!(
        "You have exited plan mode. You can now make edits, run tools, and take actions. The plan file is located at {plan} if you need to reference it."
    )));
    assert_eq!(count(&requests[4], full), 1);
}
