//! 计划模式的两个会话标志（docs/specs/rust-cli/context.md §13.4）：actor 在改模式的同一处更新，
//! 经 `current_mode_and_plan_flags` 与模式一起交给主循环，提交 reentry / exit 时清掉。

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::message::{AttachmentMessage, Message, ToolResultContent};
use rust_cli_kernel_api::model::SystemBlock;
use rust_cli_kernel_api::orchestration::{
    BuiltContext, ContextBuilder, ContextRequest, OrchestrationError,
};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::plan::{EXIT_SOURCE, PlanModeFlags, REENTRY_SOURCE};
use rust_cli_kernel_api::session_api::SessionHandle;
use rust_cli_kernel_api::testing::{ScriptedModel, text_response, tool_use_response};
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext,
};
use serde_json::{Value, json};

use crate::test_support::{Setup, finished, submit, until};

/// 输入 `{"to": "plan" | "default"}`：请求会话打开或关掉 plan（修复线 F20：plan 是独立开关，以前是改模式）。
struct SwitchMode;

#[async_trait]
impl Tool for SwitchMode {
    fn name(&self) -> &str {
        "SwitchMode"
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }

    fn description(&self, _context: &PromptContext) -> String {
        String::new()
    }

    async fn call(
        &self,
        input: Value,
        context: ToolUseContext,
        _progress: tokio::sync::mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let enabled = input["to"].as_str() == Some("plan");
        context
            .interactions
            .set_plan(&context.session, enabled)
            .await
            .map_err(|error| ToolError::Failed {
                message: error.to_string(),
            })?;
        Ok(ToolOutput {
            content: vec![ToolResultContent::Text {
                text: "ok".to_owned(),
            }],
            is_error: false,
            display: Value::Null,
        })
    }
}

/// 记下每一步看到的模式与标志；像 context 那样按标志产生 reentry / exit（不看计划文件）。
#[derive(Default)]
struct Recording(Mutex<Vec<(PermissionMode, PlanModeFlags)>>);

#[async_trait]
impl ContextBuilder for Recording {
    async fn build(
        &self,
        request: ContextRequest,
        _history: &[Message],
    ) -> Result<BuiltContext, OrchestrationError> {
        let mut seen = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        seen.push((request.mode, request.plan_flags));
        let plan = request.mode == PermissionMode::Plan;
        let source = match (plan, request.plan_flags) {
            (true, flags) if flags.has_exited => Some(REENTRY_SOURCE),
            (false, flags) if flags.needs_exit_attachment => Some(EXIT_SOURCE),
            _ => None,
        };
        let attachments = source
            .map(|source| AttachmentMessage {
                id: MessageId::new(format!("msg_attachment_{}", seen.len())),
                source: source.to_owned(),
                text: String::new(),
                data: Value::Null,
            })
            .into_iter()
            .collect();
        Ok(BuiltContext {
            system: vec![SystemBlock {
                text: "system".to_owned(),
                cache_control: None,
            }],
            user_context: Vec::new(),
            attachments,
        })
    }
}

fn flags(has_exited: bool, needs_exit_attachment: bool) -> PlanModeFlags {
    PlanModeFlags {
        has_exited,
        needs_exit_attachment,
    }
}

#[tokio::test]
async fn the_actor_owns_the_plan_flags_and_clears_them_on_commit() {
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "SwitchMode", &json!({"to": "plan"})),
        text_response(&["planning"]),
        tool_use_response("toolu_2", "SwitchMode", &json!({"to": "default"})),
        text_response(&["left"]),
        text_response(&["working"]),
        tool_use_response("toolu_3", "SwitchMode", &json!({"to": "plan"})),
        text_response(&["back"]),
        text_response(&["still planning"]),
    ]));
    let recording = Arc::new(Recording::default());
    let mut setup = Setup::new(model);
    setup.tools = vec![Arc::new(SwitchMode)];
    setup.context = Some(recording.clone());
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    for turn in 1..=5 {
        built.session.send(submit(&format!("c{turn}"), "go")).await;
        until(&mut events, finished).await;
    }
    use PermissionMode::{Default, Plan};
    let seen = recording.0.lock().unwrap().clone();
    assert_eq!(
        seen,
        [
            // 回合 1：进入 plan（标志不变）。
            (Default, flags(false, false)),
            (Plan, flags(false, false)),
            // 回合 2：离开 plan，下一步两个标志都是真；产生的 exit 提交后清掉待发标志。
            (Plan, flags(false, false)),
            (Default, flags(true, true)),
            // 回合 3：只剩“离开过”。
            (Default, flags(true, false)),
            // 回合 4：再次进入，下一步带着“离开过”；产生的 reentry 提交后清掉。
            (Default, flags(true, false)),
            (Plan, flags(true, false)),
            // 回合 5：都清了。
            (Plan, flags(false, false)),
        ]
    );
}
