use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_cli_base::models::GLM_4_6;
use rust_cli_kernel_api::env::{EnvPath, OperationContext, Origin, WorkspaceRef};
use rust_cli_kernel_api::ids::{
    EnvironmentId, Generation, InteractionId, OperationId, OwnerEpoch, RunId, SessionId, ToolUseId,
};
use rust_cli_kernel_api::orchestration::{
    HookEvent, HookEventName, HookOutcome, HookRunner, InteractionAnswer, InteractionRequest,
    Interactions, OrchestrationError, PermissionAnswer, PermissionGate,
};
use rust_cli_kernel_api::permission::{PermissionDecision, PermissionMode};
use rust_cli_kernel_api::testing::{
    FakeTool, MemoryStore, UnusedEnvironment, local_services, machine,
};
use rust_cli_kernel_api::tool::{Tool, ToolUseContext};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::*;

struct FixedGate(PermissionDecision);

#[async_trait]
impl PermissionGate for FixedGate {
    async fn decide(
        &self,
        _tool: &dyn Tool,
        _input: &Value,
        _context: &ToolUseContext,
    ) -> PermissionDecision {
        self.0.clone()
    }
}

#[derive(Default)]
struct RecordingHooks(Mutex<Vec<HookEventName>>);

#[async_trait]
impl HookRunner for RecordingHooks {
    async fn run(&self, event: HookEvent, _cancel: CancellationToken) -> HookOutcome {
        self.0.lock().unwrap().push(event.name);
        HookOutcome::default()
    }
}

struct Answer(PermissionAnswer);

#[async_trait]
impl Interactions for Answer {
    async fn ask(
        &self,
        _session: &SessionId,
        _request: InteractionRequest,
    ) -> Result<InteractionAnswer, OrchestrationError> {
        Ok(InteractionAnswer {
            interaction: InteractionId::new("i1"),
            answer: serde_json::to_value(&self.0).unwrap(),
        })
    }
}

fn context(answer: PermissionAnswer) -> ToolUseContext {
    let env = Arc::new(UnusedEnvironment);
    ToolUseContext {
        session: SessionId::new("sess_1"),
        run: RunId::new("run_1"),
        tool_use: ToolUseId::new("toolu_1"),
        agent: None,
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/work"),
        },
        mode: PermissionMode::Default,
        cancel: CancellationToken::new(),
        machine: machine(env.clone()),
        local: local_services(env),
        artifacts: Arc::new(MemoryStore::default()),
        interactions: Arc::new(Answer(answer)),
        operation: OperationContext {
            environment: EnvironmentId::new("local"),
            generation: Generation::new("g1"),
            owner_epoch: OwnerEpoch::LOCAL,
            operation: OperationId::new("op_1"),
            origin: Origin::Agent {
                session: SessionId::new("sess_1"),
                run: RunId::new("run_1"),
            },
        },
    }
}

fn allow() -> PermissionAnswer {
    PermissionAnswer::Allow {
        remember: Vec::new(),
    }
}

async fn run(
    decision: PermissionDecision,
    answer: PermissionAnswer,
    tool: FakeTool,
    input: Value,
) -> (ToolOutput, Vec<HookEventName>) {
    let hooks = Arc::new(RecordingHooks::default());
    let pipeline = Pipeline::new(Arc::new(FixedGate(decision)), hooks.clone());
    let (progress, _updates) = mpsc::channel(4);
    let output = pipeline
        .execute(Arc::new(tool), input, context(answer), progress)
        .await;
    let events = hooks.0.lock().unwrap().clone();
    (output, events)
}

fn text(output: &ToolOutput) -> String {
    match &output.content[0] {
        ToolResultContent::Text { text } => text.clone(),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn allowed_calls_run_between_pre_and_post_hooks() {
    let (output, events) = run(
        PermissionDecision::Allow {
            updated_input: None,
        },
        allow(),
        FakeTool::new("Read", true, "contents"),
        json!({}),
    )
    .await;
    assert!(!output.is_error);
    assert_eq!(text(&output), "contents");
    assert_eq!(
        events,
        vec![HookEventName::PreToolUse, HookEventName::PostToolUse]
    );
}

#[tokio::test]
async fn a_denied_call_never_runs() {
    let (output, events) = run(
        PermissionDecision::Deny {
            message: "Permission to use Bash has been denied.".to_owned(),
        },
        allow(),
        FakeTool::new("Bash", false, "out"),
        json!({}),
    )
    .await;
    assert!(output.is_error);
    assert_eq!(text(&output), "Permission to use Bash has been denied.");
    assert_eq!(events, vec![HookEventName::PreToolUse]);
}

#[tokio::test]
async fn asking_the_user_follows_the_answer() {
    let ask = || PermissionDecision::Ask {
        message: "run it?".to_owned(),
        suggestions: Vec::new(),
    };
    let (allowed, _) = run(
        ask(),
        allow(),
        FakeTool::new("Bash", false, "out"),
        json!({}),
    )
    .await;
    assert_eq!(text(&allowed), "out");
    let (denied, _) = run(
        ask(),
        PermissionAnswer::Deny {
            feedback: Some("use pnpm".to_owned()),
        },
        FakeTool::new("Bash", false, "out"),
        json!({}),
    )
    .await;
    assert!(denied.is_error);
    assert!(
        text(&denied).ends_with("the user said:\nuse pnpm"),
        "{}",
        text(&denied)
    );
}

#[tokio::test]
async fn invalid_input_is_rejected_before_any_hook() {
    struct Strict;
    #[async_trait]
    impl Tool for Strict {
        fn name(&self) -> &str {
            "Read"
        }
        fn input_schema(&self) -> Value {
            json!({"type": "object", "properties": {"file_path": {"type": "string"}}, "required": ["file_path"]})
        }
        fn description(&self, _context: &rust_cli_kernel_api::tool::PromptContext) -> String {
            String::new()
        }
        async fn call(
            &self,
            _input: Value,
            _context: ToolUseContext,
            _progress: mpsc::Sender<ToolProgress>,
        ) -> Result<ToolOutput, ToolError> {
            unreachable!()
        }
    }
    let hooks = Arc::new(RecordingHooks::default());
    let pipeline = Pipeline::new(
        Arc::new(FixedGate(PermissionDecision::Allow {
            updated_input: None,
        })),
        hooks.clone(),
    );
    let (progress, _updates) = mpsc::channel(4);
    let output = pipeline
        .execute(Arc::new(Strict), json!({}), context(allow()), progress)
        .await;
    assert_eq!(
        text(&output),
        "<tool_use_error>InputValidationError: Read failed due to the following issue:\nThe required parameter `file_path` is missing</tool_use_error>"
    );
    assert!(hooks.0.lock().unwrap().is_empty());
}

/// R5-1：输入是标记对象（原文不是 JSON）时，先于 schema、hooks 与权限给出
/// JSON 解析失败的 InputValidationError（原文取前 200 个 UTF-16 单元，长度是原文的总长度）。
#[tokio::test]
async fn unparsed_input_is_a_json_input_validation_error_before_any_hook() {
    let raw = format!("{{\"command\": \"{}", "x".repeat(300));
    let hooks = Arc::new(RecordingHooks::default());
    let pipeline = Pipeline::new(
        Arc::new(FixedGate(PermissionDecision::Allow {
            updated_input: None,
        })),
        hooks.clone(),
    );
    let (progress, _updates) = mpsc::channel(4);
    let input = json!({"__unparsedToolInput": {"raw": raw, "len": raw.len() + 5}});
    let output = pipeline
        .execute(
            Arc::new(FakeTool::new("Bash", false, "ran")),
            input,
            context(allow()),
            progress,
        )
        .await;
    let sent = &raw[..200];
    assert!(output.is_error);
    assert_eq!(
        text(&output),
        format!(
            "<tool_use_error>InputValidationError: Bash was called with input that could not be parsed as JSON.\nYou sent (first 200 of 318 bytes): {sent}\nCommon causes: unescaped backslashes in file paths (use / or \\\\), unescaped control characters, or truncated output. Retry with valid JSON.</tool_use_error>"
        )
    );
    assert!(hooks.0.lock().unwrap().is_empty());
}

#[test]
fn long_errors_keep_both_ends() {
    let message = "a".repeat(6_000) + &"b".repeat(6_000);
    let formatted = format_error(&message);
    assert!(formatted.starts_with(&"a".repeat(5_000)));
    assert!(formatted.ends_with(&"b".repeat(5_000)));
    assert!(formatted.contains("... [2000 characters truncated] ..."));
    assert_eq!(format_error("  "), "Command failed with no output");
}

/// 记下“总是允许”落库通知的判决链：每次都询问。
#[derive(Default)]
struct Remembering(Mutex<Vec<String>>);

#[async_trait]
impl PermissionGate for Remembering {
    async fn decide(
        &self,
        _tool: &dyn Tool,
        _input: &Value,
        _context: &ToolUseContext,
    ) -> PermissionDecision {
        PermissionDecision::Ask {
            message: "Run it?".to_owned(),
            suggestions: Vec::new(),
        }
    }

    async fn local_rules_committed(&self, workspace: &WorkspaceRef) {
        self.0
            .lock()
            .unwrap()
            .push(workspace.root.as_str().to_owned());
    }
}

/// “总是允许”：规则由会话 actor 落库，管线只通知判决链读回本工作区的规则集（R1-2）。
#[tokio::test]
async fn always_allow_answers_tell_the_gate_to_reload_the_workspace_rules() {
    let gate = Arc::new(Remembering::default());
    let pipeline = Pipeline::new(gate.clone(), Arc::new(RecordingHooks::default()));
    let rule = rust_cli_kernel_api::permission::PermissionRule {
        tool: "Bash".to_owned(),
        content: Some("npm test:*".to_owned()),
    };
    let answer = PermissionAnswer::Allow {
        remember: vec![rule.clone()],
    };
    let (progress, _updates) = mpsc::channel(4);
    let tool = Arc::new(FakeTool::new("Bash", false, "ran"));
    let output = pipeline
        .execute(tool, json!({}), context(answer), progress)
        .await;
    assert_eq!(text(&output), "ran");
    assert_eq!(gate.0.lock().unwrap().as_slice(), ["/work"]);
}

#[tokio::test]
async fn unanswerable_questions_return_the_question_itself() {
    let (output, _) = run(
        PermissionDecision::Ask {
            message: "touch in '/w/x' needs approval.".to_owned(),
            suggestions: Vec::new(),
        },
        PermissionAnswer::Unavailable,
        FakeTool::new("Bash", false, "ran"),
        json!({}),
    )
    .await;
    assert!(output.is_error);
    assert_eq!(text(&output), "touch in '/w/x' needs approval.");
}

/// 记下收到的询问，并一律允许。
#[derive(Default)]
struct Asked(Mutex<Vec<InteractionRequest>>);

#[async_trait]
impl Interactions for Asked {
    async fn ask(
        &self,
        _session: &SessionId,
        request: InteractionRequest,
    ) -> Result<InteractionAnswer, OrchestrationError> {
        self.0.lock().unwrap().push(request);
        Ok(InteractionAnswer {
            interaction: InteractionId::new("i1"),
            answer: json!({"decision": "allow"}),
        })
    }
}

/// 判决链的建议规则进入询问，前端据此提供"不再询问"（tui.md §5.2）。
#[tokio::test]
async fn the_ask_carries_the_suggested_rules() {
    let rule = rust_cli_kernel_api::permission::PermissionRule {
        tool: "Bash".to_owned(),
        content: Some("npm test:*".to_owned()),
    };
    let gate = Arc::new(FixedGate(PermissionDecision::Ask {
        message: "Run it?".to_owned(),
        suggestions: vec![rule.clone()],
    }));
    let pipeline = Pipeline::new(gate, Arc::new(RecordingHooks::default()));
    let asked = Arc::new(Asked::default());
    let mut context = context(PermissionAnswer::Unavailable);
    context.interactions = asked.clone();
    let (progress, _updates) = mpsc::channel(4);
    let tool = Arc::new(FakeTool::new("Bash", false, "ran"));
    let output = pipeline.execute(tool, json!({}), context, progress).await;
    assert_eq!(text(&output), "ran");
    let requests = asked.0.lock().unwrap();
    let [InteractionRequest::Permission { suggestions, .. }] = requests.as_slice() else {
        panic!("expected one permission ask: {requests:?}");
    };
    assert_eq!(suggestions.as_slice(), [rule]);
}

/// 记下 `call` 收到的输入；`strips` 为真时丢掉 schema 外的键（宽松对象），为假时按严格对象。
struct KeyedTool {
    strips: bool,
    seen: Mutex<Option<Value>>,
}

#[async_trait]
impl Tool for KeyedTool {
    fn name(&self) -> &str {
        "Keyed"
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {"prompt": {"type": "string"}},
            "required": ["prompt"],
            "additionalProperties": false
        })
    }

    fn description(&self, _context: &rust_cli_kernel_api::tool::PromptContext) -> String {
        "keyed".to_owned()
    }

    fn strips_unknown_keys(&self) -> bool {
        self.strips
    }

    async fn check_permissions(
        &self,
        _input: &Value,
        _context: &ToolUseContext,
    ) -> PermissionDecision {
        PermissionDecision::Allow {
            updated_input: None,
        }
    }

    async fn call(
        &self,
        input: Value,
        _context: ToolUseContext,
        _progress: mpsc::Sender<rust_cli_kernel_api::tool::ToolProgress>,
    ) -> Result<ToolOutput, rust_cli_kernel_api::tool::ToolError> {
        *self.seen.lock().unwrap() = Some(input);
        Ok(ToolOutput {
            content: vec![ToolResultContent::Text {
                text: "ok".to_owned(),
            }],
            is_error: false,
            display: Value::Null,
        })
    }
}

/// 修复线 F51（agents.md §2）：宽松对象的工具（Agent）丢掉 schema 外的键、不报错，工具与 hook 看到的是丢掉
/// 之后的输入；严格对象的工具（缺省）照旧报多出的参数。
#[tokio::test]
async fn object_tools_drop_unknown_keys_and_strict_tools_reject_them() {
    let input = json!({"prompt": "x", "model": GLM_4_6});
    let pipeline = Pipeline::new(
        Arc::new(FixedGate(PermissionDecision::Passthrough)),
        Arc::new(RecordingHooks::default()),
    );
    let lenient = Arc::new(KeyedTool {
        strips: true,
        seen: Mutex::new(None),
    });
    let (progress, _updates) = mpsc::channel(4);
    let output = pipeline
        .execute(lenient.clone(), input.clone(), context(allow()), progress)
        .await;
    assert!(!output.is_error, "{}", text(&output));
    assert_eq!(text(&output), "ok");
    assert_eq!(*lenient.seen.lock().unwrap(), Some(json!({"prompt": "x"})));

    let strict = Arc::new(KeyedTool {
        strips: false,
        seen: Mutex::new(None),
    });
    let (progress, _updates) = mpsc::channel(4);
    let output = pipeline
        .execute(strict.clone(), input, context(allow()), progress)
        .await;
    assert!(output.is_error);
    assert_eq!(
        text(&output),
        "<tool_use_error>InputValidationError: Keyed failed due to the following issue:\nAn unexpected parameter `model` was provided</tool_use_error>"
    );
    assert!(strict.seen.lock().unwrap().is_none());
}

/// 子代理里的询问被拒：结果是给子代理的拒绝文字（主线程是另一段，docs/specs/rust-cli/agents.md §4.1.1）。
#[tokio::test]
async fn subagent_denials_use_the_subagent_text() {
    let ask = || PermissionDecision::Ask {
        message: "run it?".to_owned(),
        suggestions: Vec::new(),
    };
    let pipeline = Pipeline::new(
        Arc::new(FixedGate(ask())),
        Arc::new(RecordingHooks::default()),
    );
    let deny = |feedback: Option<&str>| {
        let mut context = context(PermissionAnswer::Deny {
            feedback: feedback.map(str::to_owned),
        });
        context.agent = Some(rust_cli_kernel_api::ids::AgentId::new("agent_1"));
        context
    };
    let call = |context: ToolUseContext| {
        let pipeline = &pipeline;
        async move {
            let (progress, _updates) = mpsc::channel(4);
            pipeline
                .execute(
                    Arc::new(FakeTool::new("Bash", false, "out")),
                    json!({}),
                    context,
                    progress,
                )
                .await
        }
    };
    let plain = call(deny(None)).await;
    assert!(plain.is_error);
    assert_eq!(
        text(&plain),
        "Permission for this tool use was denied. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). Try a different approach or report the limitation to complete your task."
    );
    let with_feedback = call(deny(Some("use pnpm"))).await;
    assert_eq!(
        text(&with_feedback),
        "Permission for this tool use was denied. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). The user said:\nuse pnpm"
    );
    // 主线程照旧。
    let main = call(context(PermissionAnswer::Deny { feedback: None })).await;
    assert_eq!(text(&main), REJECT_MESSAGE);
    // 没有人能回答时仍是询问本身的文字（子代理也一样）。
    let mut unavailable = context(PermissionAnswer::Unavailable);
    unavailable.agent = Some(rust_cli_kernel_api::ids::AgentId::new("agent_1"));
    assert_eq!(text(&call(unavailable).await), "run it?");
}

/// 没有询问入口的后台子代理（`-p`，agents.md §4.1.1）：PermissionRequest hooks 先跑，没有结论时拒绝，模型收到
/// `Permission to use <tool> has been denied.` 加变通说明（不是询问本身的文字）。
#[tokio::test]
async fn background_agents_without_prompts_get_the_denial_text() {
    let ask = PermissionDecision::Ask {
        message: "run it?".to_owned(),
        suggestions: Vec::new(),
    };
    let (output, events) = run(
        ask,
        PermissionAnswer::NoPrompts,
        FakeTool::new("Bash", false, "out"),
        json!({}),
    )
    .await;
    assert!(output.is_error);
    assert_eq!(
        text(&output),
        format!(
            "Permission to use Bash has been denied. {}",
            rust_cli_policy::texts::denial_workaround()
        )
    );
    assert!(
        text(&output)
            .starts_with("Permission to use Bash has been denied. IMPORTANT: You *may* attempt")
    );
    assert!(
        events.contains(&HookEventName::PermissionRequest),
        "{events:?}"
    );
}
