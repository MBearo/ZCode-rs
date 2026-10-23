//! 计划文件的放行与 plan 模式的写入询问（docs/specs/rust-cli/tools-more.md §2.5）。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::env::{EnvFacts, EnvPath, OperationContext, Origin, WorkspaceRef};
use rust_cli_kernel_api::ids::{
    EnvironmentId, Generation, InteractionId, OperationId, OwnerEpoch, RunId, SessionId, ToolUseId,
};
use rust_cli_kernel_api::orchestration::{
    InteractionAnswer, InteractionRequest, Interactions, OrchestrationError, PermissionGate,
};
use rust_cli_kernel_api::permission::{PermissionDecision, PermissionMode, RuleBehavior};
use rust_cli_kernel_api::testing::{MemoryEnvironment, MemoryStore};
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::test_support::rules;
use crate::{PolicyGate, PolicyOptions, texts};

const PLAN: &str = "/w/.zcode/plans/plan-s1.md";

struct NoAnswers;

#[async_trait]
impl Interactions for NoAnswers {
    async fn ask(
        &self,
        _session: &SessionId,
        _request: InteractionRequest,
    ) -> Result<InteractionAnswer, OrchestrationError> {
        Ok(InteractionAnswer {
            interaction: InteractionId::new("i"),
            answer: json!({"decision": "unavailable"}),
        })
    }
}

/// 没有自身检查的内置文件工具（Write / Edit / Read）。
struct Builtin(&'static str);

#[async_trait]
impl Tool for Builtin {
    fn name(&self) -> &str {
        self.0
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn description(&self, _context: &PromptContext) -> String {
        String::new()
    }
    async fn call(
        &self,
        _input: Value,
        _context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        Err(ToolError::Unsupported {
            what: "test".to_owned(),
        })
    }
}

fn context(mode: PermissionMode, home: Option<&str>) -> ToolUseContext {
    let env = MemoryEnvironment::new(EnvFacts {
        cwd: EnvPath::new("/w"),
        platform: "linux".to_owned(),
        os_version: "Linux 6.8.0".to_owned(),
        shell: "bash".to_owned(),
        is_git_repo: false,
        additional_directories: Vec::new(),
        home: home.map(EnvPath::new),
    });
    let session = SessionId::new("s1");
    ToolUseContext {
        session: session.clone(),
        run: RunId::new("r1"),
        tool_use: ToolUseId::new("toolu_1"),
        agent: None,
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/w"),
        },
        mode,
        cancel: CancellationToken::new(),
        env: Arc::new(env),
        artifacts: Arc::new(MemoryStore::default()),
        interactions: Arc::new(NoAnswers),
        operation: OperationContext {
            environment: EnvironmentId::new("local"),
            generation: Generation::new("g1"),
            owner_epoch: OwnerEpoch::LOCAL,
            operation: OperationId::new("op_1"),
            origin: Origin::Agent {
                session,
                run: RunId::new("r1"),
            },
        },
    }
}

async fn decide(
    gate: &PolicyGate,
    tool: &'static str,
    path: &str,
    mode: PermissionMode,
) -> PermissionDecision {
    let input = json!({"file_path": path, "content": "x"});
    gate.decide(&Builtin(tool), &input, &context(mode, Some("/home/dev")))
        .await
}

fn allowed(decision: &PermissionDecision) -> bool {
    matches!(decision, PermissionDecision::Allow { .. })
}

#[tokio::test]
async fn plan_mode_allows_writing_and_reading_this_sessions_plan_files() {
    let gate = PolicyGate::default();
    for tool in ["Write", "Edit", "Read"] {
        let decision = decide(&gate, tool, PLAN, PermissionMode::Plan).await;
        assert!(allowed(&decision), "{tool}: {decision:?}");
    }
    // 不看模式：default、acceptEdits 下写计划文件也不询问（计划目录在工作区的 `.zcode` 下，放行排在
    // 安全路径之前，不按敏感文件询问）。
    for mode in [PermissionMode::Default, PermissionMode::AcceptEdits] {
        assert!(
            allowed(&decide(&gate, "Write", PLAN, mode).await),
            "{mode:?}"
        );
    }
    // 计划文件在工作区里，不依赖 home。
    let input = json!({"file_path": PLAN, "content": "x"});
    let decision = gate
        .decide(
            &Builtin("Write"),
            &input,
            &context(PermissionMode::Plan, None),
        )
        .await;
    assert!(allowed(&decision), "{decision:?}");
}

#[tokio::test]
async fn other_writes_stay_blocked_in_plan_mode() {
    let gate = PolicyGate::default();
    assert_eq!(
        decide(&gate, "Write", "/w/src/a.rs", PermissionMode::Plan).await,
        PermissionDecision::Deny {
            message: texts::CLASSIFIER_UNAVAILABLE.to_owned()
        }
    );
    // 别的会话（含子代理的子会话）的计划文件、旧的文件名都不放行：它们在 `.zcode` 下，按安全路径
    // 询问（不经分类器）。
    for path in [
        "/w/.zcode/plans/plan-s2.md",
        "/w/.zcode/plans/plan-sess_subagent_agent_a1.md",
        "/w/.zcode/plans/plan-s1-agent-a1.md",
        "/w/.zcode/plans/s1.md",
        "/w/.zcode/plans/plan-s1.txt",
    ] {
        assert_eq!(
            decide(&gate, "Write", path, PermissionMode::Plan).await,
            PermissionDecision::Ask {
                message: texts::file_sensitive(path),
                suggestions: Vec::new(),
            },
            "{path}"
        );
    }
}

#[tokio::test]
async fn without_the_classifier_plan_mode_asks_with_the_plan_mode_text() {
    let gate = PolicyGate::default().with_options(PolicyOptions {
        auto_in_plan: false,
    });
    let decision = decide(&gate, "Write", "/w/src/a.rs", PermissionMode::Plan).await;
    assert_eq!(
        decision,
        PermissionDecision::Ask {
            message: "Cannot write to /w/src/a.rs while in plan mode.".to_owned(),
            suggestions: Vec::new(),
        }
    );
}

#[tokio::test]
async fn edit_allow_rules_no_longer_override_plan_mode() {
    let gate = PolicyGate::new(rules(&[(RuleBehavior::Allow, "Edit(src/**)")])).with_options(
        PolicyOptions {
            auto_in_plan: false,
        },
    );
    let path = "/w/src/a.rs";
    assert!(allowed(
        &decide(&gate, "Write", path, PermissionMode::Default).await
    ));
    assert!(matches!(
        decide(&gate, "Write", path, PermissionMode::Plan).await,
        PermissionDecision::Ask { .. }
    ));
}

#[tokio::test]
async fn deny_and_ask_rules_still_apply_to_the_plan_file() {
    let denied = PolicyGate::new(rules(&[(RuleBehavior::Deny, "Edit(//w/.zcode/**)")]));
    assert_eq!(
        decide(&denied, "Write", PLAN, PermissionMode::Plan).await,
        PermissionDecision::Deny {
            message: texts::file_edit_denied(PLAN)
        }
    );
    let asked = PolicyGate::new(rules(&[(RuleBehavior::Ask, "Edit(//w/.zcode/**)")])).with_options(
        PolicyOptions {
            auto_in_plan: false,
        },
    );
    assert_eq!(
        decide(&asked, "Write", PLAN, PermissionMode::Plan).await,
        PermissionDecision::Ask {
            message: texts::file_write_request(PLAN),
            suggestions: Vec::new(),
        }
    );
}
