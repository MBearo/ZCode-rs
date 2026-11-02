//! 管线里的 hooks（docs/specs/rust-cli/hooks.md §6）：PreToolUse 与判决链的先后、PermissionRequest、
//! PostToolUse 的替换与消息、停止请求。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_cli_kernel_api::env::{EnvPath, OperationContext, Origin, WorkspaceRef};
use rust_cli_kernel_api::hooks::{HookEvent, HookOutcome};
use rust_cli_kernel_api::ids::{
    EnvironmentId, Generation, MessageId, OperationId, RunId, SessionId,
};
use rust_cli_kernel_api::message::{AttachmentMessage, Message};
use rust_cli_kernel_api::orchestration::InteractionRequest;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::{
    ScriptedInteractions, UnusedEnvironment, controller_services, machine,
};
use rust_cli_kernel_api::tool::PromptContext;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::*;

/// 回显 `command` 的工具；schema 要求 `command` 是字符串。
struct Echo;

#[async_trait]
impl Tool for Echo {
    fn name(&self) -> &str {
        "Bash"
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object", "properties": {"command": {"type": "string"}}, "required": ["command"]})
    }
    fn description(&self, _context: &PromptContext) -> String {
        String::new()
    }
    async fn call(
        &self,
        input: Value,
        _context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput {
            content: vec![ToolResultContent::Text {
                text: format!("ran {}", input["command"].as_str().unwrap_or_default()),
            }],
            is_error: false,
            display: Value::Null,
        })
    }
}

/// 判决链替身：`decide` 与 `check_rules` 都是固定值，并记录 decide 是否被调用。
struct Gate {
    decide: PermissionDecision,
    rules: Option<PermissionDecision>,
    decided: Mutex<u32>,
}

impl Gate {
    fn new(decide: PermissionDecision, rules: Option<PermissionDecision>) -> Arc<Self> {
        Arc::new(Self {
            decide,
            rules,
            decided: Mutex::new(0),
        })
    }
}

#[async_trait]
impl PermissionGate for Gate {
    async fn decide(&self, _t: &dyn Tool, _i: &Value, _c: &ToolUseContext) -> PermissionDecision {
        *self.decided.lock().unwrap() += 1;
        self.decide.clone()
    }
    async fn local_rules_committed(&self, _w: &WorkspaceRef) {}
    async fn check_rules(
        &self,
        _t: &dyn Tool,
        _i: &Value,
        _c: &ToolUseContext,
    ) -> Option<PermissionDecision> {
        self.rules.clone()
    }
}

/// 按事件名回答固定结果的 hooks，并记录收到的事件。
#[derive(Default)]
struct Hooks {
    outcomes: HashMap<HookEventName, HookOutcome>,
    seen: Mutex<Vec<HookEvent>>,
}

#[async_trait]
impl HookRunner for Hooks {
    async fn run(&self, event: HookEvent, _cancel: CancellationToken) -> HookOutcome {
        let outcome = self.outcomes.get(&event.name).cloned().unwrap_or_default();
        self.seen.lock().unwrap().push(event);
        outcome
    }
}

fn context(asked: Arc<ScriptedInteractions>) -> ToolUseContext {
    let env = Arc::new(UnusedEnvironment);
    ToolUseContext {
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/work"),
        },
        mode: PermissionMode::AcceptEdits,
        machine: machine(env.clone()),
        controller: controller_services(env),
        interactions: asked,
        session_ops: Arc::new(rust_cli_kernel_api::testing::NoSession),
        operation: OperationContext::new(
            EnvironmentId::new("local"),
            Generation::new("g1"),
            OperationId::new("op_1"),
            Origin::Agent {
                session: SessionId::new("sess_1"),
                run: RunId::new("run_1"),
            },
        ),
        ..rust_cli_kernel_api::testing::tool_use_context()
    }
}

fn note(source: &str, text: &str) -> Message {
    Message::Attachment(AttachmentMessage {
        id: MessageId::new(text),
        source: source.to_owned(),
        text: text.to_owned(),
        data: Value::Null,
    })
}

struct Ran {
    execution: ToolExecution,
    asked: Vec<String>,
    decided: u32,
    events: Vec<HookEvent>,
}

async fn run(gate: Arc<Gate>, outcomes: &[(HookEventName, HookOutcome)]) -> Ran {
    let hooks = Arc::new(Hooks {
        outcomes: outcomes.iter().cloned().collect(),
        ..Hooks::default()
    });
    let asked = Arc::new(ScriptedInteractions::allowing());
    let pipeline = Pipeline::new(gate.clone(), hooks.clone());
    let (progress, _updates) = mpsc::channel(4);
    let execution = pipeline
        .execute(
            Arc::new(Echo),
            json!({"command": "ls"}),
            context(asked.clone()),
            progress,
        )
        .await;
    let events = hooks.seen.lock().unwrap().drain(..).collect();
    let decided = *gate.decided.lock().unwrap();
    let asked = asked
        .asked()
        .into_iter()
        .filter_map(|request| match request {
            InteractionRequest::Permission { message, .. } => Some(message),
            _ => None,
        })
        .collect();
    Ran {
        execution,
        asked,
        decided,
        events,
    }
}

fn text(output: &ToolOutput) -> String {
    match &output.content[0] {
        ToolResultContent::Text { text } => text.clone(),
        other => panic!("{other:?}"),
    }
}

fn ask_gate() -> Arc<Gate> {
    Gate::new(PermissionDecision::ask("chain asks", Vec::new()), None)
}

fn pre(
    decision: Option<PermissionDecision>,
    updated: Option<Value>,
) -> (HookEventName, HookOutcome) {
    (
        HookEventName::PreToolUse,
        HookOutcome {
            decision,
            updated_input: updated,
            ..HookOutcome::default()
        },
    )
}

#[tokio::test]
async fn hook_allow_skips_the_prompt_unless_rules_say_otherwise() {
    let allow = Some(PermissionDecision::Allow {
        updated_input: None,
    });
    let ran = run(
        ask_gate(),
        &[pre(allow.clone(), Some(json!({"command": "pwd"})))],
    )
    .await;
    assert_eq!(text(&ran.execution.output), "ran pwd");
    assert!(ran.asked.is_empty() && ran.decided == 0);
    // deny 规则仍然生效。
    let denied = Gate::new(
        PermissionDecision::Passthrough,
        Some(PermissionDecision::Deny {
            message: "rule says no".to_owned(),
        }),
    );
    let ran = run(denied, &[pre(allow.clone(), None)]).await;
    assert_eq!(text(&ran.execution.output), "rule says no");
    // ask 规则：走完整判决链。
    let rule_ask = Gate::new(
        PermissionDecision::ask("chain asks", Vec::new()),
        Some(PermissionDecision::ask("rule", Vec::new())),
    );
    let ran = run(rule_ask, &[pre(allow, None)]).await;
    assert_eq!(
        (ran.decided, ran.asked.as_slice()),
        (1, ["chain asks".to_owned()].as_slice())
    );
}

#[tokio::test]
async fn hook_ask_and_deny() {
    let ask = Some(PermissionDecision::ask("hook asks", Vec::new()));
    let ran = run(ask_gate(), &[pre(ask, None)]).await;
    assert_eq!(ran.asked, ["hook asks"]);
    assert_eq!(ran.decided, 0);
    let deny = Some(PermissionDecision::Deny {
        message: "PreToolUse:Bash hook error: [g]: no".to_owned(),
    });
    let ran = run(ask_gate(), &[pre(deny, None)]).await;
    assert!(ran.execution.output.is_error);
    assert_eq!(
        text(&ran.execution.output),
        "PreToolUse:Bash hook error: [g]: no"
    );
    // 没有决定：完整判决链，passthrough 的 updatedInput 生效。
    let ran = run(ask_gate(), &[pre(None, Some(json!({"command": "whoami"})))]).await;
    assert_eq!(
        (ran.decided, text(&ran.execution.output).as_str()),
        (1, "ran whoami")
    );
}

#[tokio::test]
async fn invalid_updated_input_is_denied() {
    let ran = run(ask_gate(), &[pre(None, Some(json!({"command": 1})))]).await;
    assert!(text(&ran.execution.output).starts_with(
        "PreToolUse hook for Bash returned updatedInput that failed schema validation: "
    ));
}

#[tokio::test]
async fn permission_request_hooks_answer_before_the_user() {
    let allow = (
        HookEventName::PermissionRequest,
        HookOutcome {
            decision: Some(PermissionDecision::Allow {
                updated_input: None,
            }),
            updated_input: Some(json!({"command": "id"})),
            ..HookOutcome::default()
        },
    );
    let ran = run(ask_gate(), &[allow]).await;
    assert_eq!(text(&ran.execution.output), "ran id");
    assert!(ran.asked.is_empty());
    let deny = (
        HookEventName::PermissionRequest,
        HookOutcome {
            decision: Some(PermissionDecision::Deny {
                message: "Permission denied by hook".to_owned(),
            }),
            ..HookOutcome::default()
        },
    );
    let ran = run(ask_gate(), &[deny]).await;
    assert_eq!(text(&ran.execution.output), "Permission denied by hook");
    let names: Vec<_> = ran.events.iter().map(|e| e.name).collect();
    assert_eq!(
        names,
        [HookEventName::PreToolUse, HookEventName::PermissionRequest]
    );
    assert_eq!(
        ran.events[1].payload,
        json!({"tool_name": "Bash", "tool_input": {"command": "ls"}})
    );
}

#[tokio::test]
async fn hook_messages_and_stops_surround_the_result() {
    let allow_gate = Gate::new(
        PermissionDecision::Allow {
            updated_input: None,
        },
        None,
    );
    let pre_outcome = HookOutcome {
        messages: vec![note("hook_additional_context", "pre ctx")],
        after_result: vec![note("hook_stopped_continuation", "pre stop")],
        prevent_continuation: true,
        ..HookOutcome::default()
    };
    let post_outcome = HookOutcome {
        messages: vec![note("hook_additional_context", "post ctx")],
        updated_tool_output: Some(json!("rewritten")),
        ..HookOutcome::default()
    };
    let ran = run(
        allow_gate,
        &[
            (HookEventName::PreToolUse, pre_outcome),
            (HookEventName::PostToolUse, post_outcome),
        ],
    )
    .await;
    let ids = |messages: &[Message]| {
        messages
            .iter()
            .map(|m| m.id().as_str().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(text(&ran.execution.output), "rewritten");
    assert_eq!(ids(&ran.execution.before), ["pre ctx"]);
    assert_eq!(ids(&ran.execution.after), ["post ctx", "pre stop"]);
    assert!(ran.execution.prevent_continuation);
    // 公共字段：模式与运行。
    let context = &ran.events[0].context;
    assert_eq!(context.mode, Some(PermissionMode::AcceptEdits));
    assert_eq!(context.run.as_ref().map(RunId::as_str), Some("run_1"));
    assert_eq!(
        ran.events[1].payload["tool_response"],
        json!([{"type": "text", "text": "ran ls"}])
    );
}

#[tokio::test]
async fn non_hook_rejections_drop_the_pre_tool_stop() {
    let deny_gate = Gate::new(
        PermissionDecision::Deny {
            message: "chain says no".to_owned(),
        },
        None,
    );
    let pre_outcome = HookOutcome {
        after_result: vec![note("hook_stopped_continuation", "pre stop")],
        prevent_continuation: true,
        ..HookOutcome::default()
    };
    let ran = run(deny_gate, &[(HookEventName::PreToolUse, pre_outcome)]).await;
    assert_eq!(text(&ran.execution.output), "chain says no");
    assert!(ran.execution.after.is_empty() && !ran.execution.prevent_continuation);
}

/// 修复线 F20：guarded 的危险命令只批准这一次（Node user-once）：PermissionRequest hook 不能替用户批准，
/// 不跑这个事件，直接问用户。工作线 F137：判决链的询问带 `once`（以前是单独的 `ask_once`）。
#[tokio::test]
async fn ask_once_questions_skip_permission_request_hooks() {
    let allow = (
        HookEventName::PermissionRequest,
        HookOutcome {
            decision: Some(PermissionDecision::Allow {
                updated_input: None,
            }),
            ..HookOutcome::default()
        },
    );
    let gate = Gate::new(
        PermissionDecision::Ask {
            message: "Dangerous command requires your approval for this execution".to_owned(),
            suggestions: Vec::new(),
            once: true,
        },
        None,
    );
    let ran = run(gate, &[allow]).await;
    assert_eq!(
        ran.asked,
        ["Dangerous command requires your approval for this execution"]
    );
    let names: Vec<_> = ran.events.iter().map(|e| e.name).collect();
    assert!(
        !names.contains(&HookEventName::PermissionRequest),
        "{names:?}"
    );
}
