//! 判决链的每个分支：整个工具的规则、工具自身的检查、六种模式、分类器、没有人回答。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::env::{EnvPath, OperationContext, Origin, WorkspaceRef};
use rust_cli_kernel_api::ids::{
    EnvironmentId, Generation, InteractionId, OperationId, OwnerEpoch, RunId, SessionId, ToolUseId,
};
use rust_cli_kernel_api::orchestration::{
    InteractionAnswer, InteractionRequest, Interactions, OrchestrationError,
};
use rust_cli_kernel_api::permission::RuleBehavior;
use rust_cli_kernel_api::testing::{MemoryStore, UnusedEnvironment};
use rust_cli_kernel_api::tool::{ToolError, ToolOutput, ToolProgress};
use serde_json::json;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::classifier::{ClassifierRequest, ClassifierVerdict};
use crate::test_support::rules;
use crate::texts;

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

/// 一个名字可选、自身检查可选的工具。
struct Named(&'static str, PermissionDecision);

#[async_trait]
impl Tool for Named {
    fn name(&self) -> &str {
        self.0
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn description(&self, _context: &rust_cli_kernel_api::tool::PromptContext) -> String {
        String::new()
    }
    async fn check_permissions(
        &self,
        _input: &Value,
        _context: &ToolUseContext,
    ) -> PermissionDecision {
        self.1.clone()
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

fn context(mode: PermissionMode) -> ToolUseContext {
    ToolUseContext {
        session: SessionId::new("s1"),
        run: RunId::new("r1"),
        tool_use: ToolUseId::new("toolu_1"),
        agent: None,
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/w"),
        },
        mode,
        cancel: CancellationToken::new(),
        env: Arc::new(UnusedEnvironment),
        artifacts: Arc::new(MemoryStore::default()),
        interactions: Arc::new(NoAnswers),
        operation: OperationContext {
            environment: EnvironmentId::new("local"),
            generation: Generation::new("g"),
            owner_epoch: OwnerEpoch::LOCAL,
            operation: OperationId::new("op"),
            origin: Origin::Agent {
                session: SessionId::new("s1"),
                run: RunId::new("r1"),
            },
        },
    }
}

async fn decide(
    gate: &PolicyGate,
    mode: PermissionMode,
    tool: &'static str,
    input: Value,
) -> PermissionDecision {
    gate.decide(
        &Named(tool, PermissionDecision::Passthrough),
        &input,
        &context(mode),
    )
    .await
}

async fn bash(gate: &PolicyGate, mode: PermissionMode, command: &str) -> PermissionDecision {
    decide(gate, mode, "Bash", json!({"command": command})).await
}

fn allow() -> PermissionDecision {
    PermissionDecision::Allow {
        updated_input: None,
    }
}

fn deny(message: impl Into<String>) -> PermissionDecision {
    PermissionDecision::Deny {
        message: message.into(),
    }
}

fn ask_message(decision: &PermissionDecision) -> &str {
    match decision {
        PermissionDecision::Ask { message, .. } => message,
        other => panic!("not ask: {other:?}"),
    }
}

const TOUCH_INSIDE: &str = "touch in '/w/x' needs approval. The path is inside the working directories for this session ('/w'), and ZCode asks before a shell command creates, changes or removes files there.";

#[tokio::test]
async fn default_mode_allows_read_only_and_asks_for_the_rest() {
    let gate = PolicyGate::default();
    assert_eq!(
        bash(&gate, PermissionMode::Default, "echo hi").await,
        allow()
    );
    let asked = bash(&gate, PermissionMode::Default, "touch x").await;
    assert_eq!(ask_message(&asked), TOUCH_INSIDE);
    let PermissionDecision::Ask { suggestions, .. } =
        bash(&gate, PermissionMode::Default, "npm --version").await
    else {
        panic!("ask");
    };
    assert_eq!(suggestions[0].content.as_deref(), Some("npm:*"));
}

#[tokio::test]
async fn dont_ask_turns_every_question_into_a_denial() {
    let gate = PolicyGate::default();
    assert_eq!(bash(&gate, PermissionMode::DontAsk, "ls").await, allow());
    assert_eq!(
        bash(&gate, PermissionMode::DontAsk, "touch x").await,
        deny(texts::dont_ask_rejected("Bash"))
    );
}

#[tokio::test]
async fn whole_tool_rules_come_first_and_last() {
    let denied = PolicyGate::new(rules(&[(RuleBehavior::Deny, "Bash")]));
    assert_eq!(
        bash(&denied, PermissionMode::BypassPermissions, "echo hi").await,
        deny("Permission to use Bash has been denied.")
    );
    let asked = PolicyGate::new(rules(&[(RuleBehavior::Ask, "Bash")]));
    assert_eq!(
        ask_message(&bash(&asked, PermissionMode::BypassPermissions, "echo hi").await),
        "ZCode requested permissions to use Bash, but you haven't granted it yet."
    );
    let allowed = PolicyGate::new(rules(&[(RuleBehavior::Allow, "Bash")]));
    assert_eq!(
        bash(&allowed, PermissionMode::Default, "touch x").await,
        allow()
    );
    assert_eq!(
        bash(&allowed, PermissionMode::Default, "mkdir -p a && cd a").await,
        allow()
    );
}

#[tokio::test]
async fn bypass_still_respects_deny_and_ask_rules() {
    let gate = PolicyGate::new(rules(&[
        (RuleBehavior::Deny, "Bash(rm:*)"),
        (RuleBehavior::Ask, "Bash(git push:*)"),
    ]));
    let mode = PermissionMode::BypassPermissions;
    assert_eq!(bash(&gate, mode, "touch /etc/x").await, allow());
    assert_eq!(
        bash(&gate, mode, "rm -f a").await,
        deny(texts::bash_command_denied("rm -f a"))
    );
    assert!(matches!(
        bash(&gate, mode, "git push").await,
        PermissionDecision::Ask { .. }
    ));
}

#[tokio::test]
async fn accept_edits_only_covers_the_working_directories() {
    let gate = PolicyGate::default();
    let mode = PermissionMode::AcceptEdits;
    assert_eq!(
        decide(&gate, mode, "Edit", json!({"file_path": "/w/src/a.rs"})).await,
        allow()
    );
    assert!(matches!(
        decide(&gate, mode, "Edit", json!({"file_path": "/tmp/a.rs"})).await,
        PermissionDecision::Ask { .. }
    ));
    assert_eq!(bash(&gate, mode, "mkdir a && touch a/b").await, allow());
    // 读工作目录以外要询问（任何模式）。
    assert_eq!(
        ask_message(
            &decide(
                &gate,
                PermissionMode::Default,
                "Read",
                json!({"file_path": "/etc/hosts"})
            )
            .await
        ),
        "ZCode requested permissions to read from /etc/hosts, but you haven't granted it yet."
    );
    assert_eq!(
        decide(
            &gate,
            PermissionMode::Default,
            "Glob",
            json!({"pattern": "*.rs"})
        )
        .await,
        allow()
    );
}

#[tokio::test]
async fn plan_mode_sends_questions_to_the_classifier() {
    let gate = PolicyGate::default();
    assert_eq!(
        bash(&gate, PermissionMode::Plan, "echo planning").await,
        allow()
    );
    // 没有分类器时按“不可用”拦下。
    assert_eq!(
        bash(&gate, PermissionMode::Plan, "touch plan.txt").await,
        deny(texts::CLASSIFIER_UNAVAILABLE)
    );
    let classic = PolicyGate::default().with_options(PolicyOptions {
        auto_in_plan: false,
    });
    assert!(matches!(
        bash(&classic, PermissionMode::Plan, "touch plan.txt").await,
        PermissionDecision::Ask { .. }
    ));
}

#[path = "lib_tests_more.rs"]
mod more;

#[path = "lib_tests_guarded.rs"]
mod guarded_chain;

#[path = "lib_tests_find.rs"]
mod find_paths;

#[path = "lib_tests_safety.rs"]
mod safety;
