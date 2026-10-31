//! Skill 工具（docs/specs/rust-cli/ext.md §2.4）。描述与 schema 在 `prompts/tools/Skill.json`。
//!
//! inline 执行：tool_result 是 `Launching skill: <名字>`，skill 正文作为 meta user 消息排在其后
//! （[`Tool::call_with_messages`]）。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::clock::Ids;
use rust_cli_kernel_api::message::{
    ContentBlock, Message, ToolResultContent, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::permission::PermissionDecision;
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolCallOutput, ToolError, ToolOutput, ToolProgress, ToolUseContext,
    Validation,
};
use serde_json::Value;
use tokio::sync::mpsc;

use super::inline::Runner;
use super::{Skill, SkillCatalog};

/// 工具名。
pub(crate) const NAME: &str = "Skill";
/// skill 正文消息的来源标签（meta user 消息，node-store 按原文保存）。
pub const SKILL_MESSAGE_SOURCE: &str = "skill";
/// 校验失败的错误码：写法不对、没有这个 skill、不许模型调用。
const INVALID_FORMAT: i32 = 1;
const UNKNOWN_SKILL: i32 = 2;
const MODEL_INVOCATION_DISABLED: i32 = 4;
/// 结果的字符上限（通用值只定义在 kernel-api）。
const MAX_RESULT_CHARS: usize = rust_cli_kernel_api::tool::DEFAULT_MAX_RESULT_CHARS;

pub struct SkillTool {
    catalog: Arc<SkillCatalog>,
    ids: Arc<dyn Ids>,
    /// `!` 命令的执行者；注册表里没有 Bash 时为 None（命令原样保留）。
    runner: Option<Runner>,
}

impl SkillTool {
    pub(crate) fn new(
        catalog: Arc<SkillCatalog>,
        ids: Arc<dyn Ids>,
        runner: Option<Runner>,
    ) -> Self {
        Self {
            catalog,
            ids,
            runner,
        }
    }
}

/// 模型给的名字：去掉首尾空白与前导 `/`。
fn requested(input: &Value) -> (String, String) {
    let raw = input
        .get("skill")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let trimmed = raw.trim();
    let name = trimmed.strip_prefix('/').unwrap_or(trimmed);
    (raw.to_owned(), name.to_owned())
}

/// 编辑距离不超过 2 的最近的 skill 名（`maxEditDistance: 2`；算法在
/// `closest.rs`，F58）。
fn closest(name: &str, skills: &[Arc<Skill>]) -> Option<String> {
    let names = skills.iter().map(|skill| skill.name.as_str());
    super::closest::closest(name, names).map(str::to_owned)
}

fn invalid(message: String, code: i32) -> Validation {
    Validation::Invalid {
        message,
        error_code: Some(code),
    }
}

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        NAME
    }

    fn input_schema(&self) -> Value {
        rust_cli_prompts::tool(NAME)
            .map(|prompt| prompt.input_schema.clone())
            .unwrap_or(Value::Null)
    }

    fn description(&self, _context: &PromptContext) -> String {
        rust_cli_prompts::tool(NAME)
            .map(|prompt| prompt.description.clone())
            .unwrap_or_default()
    }

    fn max_result_size_chars(&self) -> Option<usize> {
        Some(MAX_RESULT_CHARS)
    }

    async fn validate_input(&self, input: &Value, context: &ToolUseContext) -> Validation {
        let (raw, name) = requested(input);
        if raw.trim().is_empty() {
            return invalid(format!("Invalid skill format: {raw}"), INVALID_FORMAT);
        }
        let skills = self.catalog.skills(&context.workspace).await;
        let Some(skill) = skills.iter().find(|skill| skill.name == name) else {
            let message = match closest(&name, &skills) {
                Some(close) => format!("Unknown skill: {name}. Did you mean {close}?"),
                None => format!("Unknown skill: {name}"),
            };
            return invalid(message, UNKNOWN_SKILL);
        };
        if skill.disable_model_invocation {
            return invalid(
                format!(
                    "Skill {name} cannot be used with {NAME} tool due to disable-model-invocation. Ask the user to run /{name} themselves \u{2014} it cannot be invoked via the {NAME} tool. Do not replicate this skill's workflow by other means \u{2014} it is reserved for explicit user invocation."
                ),
                MODEL_INVOCATION_DISABLED,
            );
        }
        Validation::Ok
    }

    /// 只有安全字段的 skill 放行；其余交给判决链（`Skill(...)` 规则与询问在 policy，ext.md §2.4）。
    async fn check_permissions(
        &self,
        input: &Value,
        context: &ToolUseContext,
    ) -> PermissionDecision {
        let (_, name) = requested(input);
        match self.catalog.find(&context.workspace, &name).await {
            Some(skill) if skill.is_safe() => PermissionDecision::Allow {
                updated_input: None,
            },
            _ => PermissionDecision::Passthrough,
        }
    }

    async fn call(
        &self,
        input: Value,
        context: ToolUseContext,
        progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        Ok(self
            .call_with_messages(input, context, progress)
            .await?
            .output)
    }

    async fn call_with_messages(
        &self,
        input: Value,
        context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolCallOutput, ToolError> {
        let (_, name) = requested(&input);
        let Some(skill) = self.catalog.find(&context.workspace, &name).await else {
            return Err(ToolError::Failed {
                message: format!("Unknown skill: {name}"),
            });
        };
        let args = input
            .get("args")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let content =
            super::body::render(&self.catalog, self.runner.as_ref(), &skill, args, &context)
                .await
                .map_err(|message| ToolError::Failed { message })?;
        Ok(self.launched(&name, content))
    }
}

impl SkillTool {
    /// tool_result 是 `Launching skill: <名字>`，正文作为 meta user 消息排在其后。
    fn launched(&self, name: &str, content: String) -> ToolCallOutput {
        let output = ToolOutput {
            content: vec![ToolResultContent::Text {
                text: format!("Launching skill: {name}"),
            }],
            is_error: false,
            display: Value::Null,
        };
        let message = Message::User(UserMessage {
            id: self.ids.message(),
            content: vec![ContentBlock::Text { text: content }],
            origin: UserOrigin::Meta {
                source: SKILL_MESSAGE_SOURCE.to_owned(),
            },
        });
        ToolCallOutput {
            output,
            new_messages: vec![message],
        }
    }
}

#[cfg(test)]
#[path = "tool_tests.rs"]
mod tests;
