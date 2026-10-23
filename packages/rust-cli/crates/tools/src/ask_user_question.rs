//! AskUserQuestion（docs/specs/rust-cli/tools-more.md §1）。
//!
//! 提问本身就是这个工具的权限询问；本工程把它表达成 `InteractionRequest::Question`，
//! 判决链直接放行，在 `call` 里问用户、按回答拼出结果文本。

use std::collections::{BTreeMap, HashSet};

use async_trait::async_trait;
use rust_cli_kernel_api::orchestration::{InteractionRequest, QuestionAnswer};
use rust_cli_kernel_api::permission::{PermissionDecision, PermissionMode};
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext, Validation,
};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::common::{Definition, definition, invalid, text};
use crate::pipeline::{REJECT_MESSAGE, rejected};
use crate::texts_more as texts;

pub const NAME: &str = "AskUserQuestion";

/// 只给多选答案拼接时用的分隔符（`answers` 用 `, ` 连接）。
const SEPARATOR: &str = ", ";
/// 只有备注、没有选项时的占位答案。
const NOTES_ONLY: &str = "(notes only)";

pub struct AskUserQuestionTool {
    definition: Definition,
    /// 只在交互会话里可用（非交互且没有权限提示工具时禁用）。
    interactive: bool,
}

impl AskUserQuestionTool {
    pub fn new(interactive: bool) -> Self {
        Self {
            definition: definition(NAME),
            interactive,
        }
    }
}

fn questions(input: &Value) -> &[Value] {
    input
        .get("questions")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

fn labels(question: &Value) -> Vec<&str> {
    question
        .get("options")
        .and_then(Value::as_array)
        .map(|options| {
            options
                .iter()
                .filter_map(|option| option.get("label").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default()
}

fn question_text(question: &Value) -> &str {
    question
        .get("question")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// schema 里校验器覆盖不到的约束：数量范围与唯一性。
fn check(input: &Value) -> Option<String> {
    let questions = questions(input);
    if questions.iter().any(|question| labels(question).len() < 2) {
        // 引导语排在校验错误之前。
        return Some(format!(
            "InputValidationError: {}\n\nquestions: every question needs 2-4 options",
            texts::SINGLE_OPTION_STEER
        ));
    }
    if questions.is_empty() || questions.len() > 4 {
        return Some("InputValidationError: questions: expected 1-4 questions".to_owned());
    }
    if questions.iter().any(|question| labels(question).len() > 4) {
        return Some("InputValidationError: options: expected 2-4 options".to_owned());
    }
    let mut texts_seen = HashSet::new();
    let unique_questions = questions
        .iter()
        .all(|question| texts_seen.insert(question_text(question)));
    let unique_labels = questions.iter().all(|question| {
        let labels = labels(question);
        labels.iter().collect::<HashSet<_>>().len() == labels.len()
    });
    (!unique_questions || !unique_labels)
        .then(|| format!("InputValidationError: {}", texts::QUESTIONS_NOT_UNIQUE))
}

/// 把多选的 `, ` 连接文本拆回标签（含 `"` 或 `, ` 的标签是 JSON 字符串）。
fn split(answer: &str) -> Option<Vec<String>> {
    let mut items = Vec::new();
    let mut rest = answer;
    loop {
        if rest.starts_with('"') {
            let mut escaped = false;
            let end = rest.char_indices().skip(1).find_map(|(index, ch)| {
                if escaped {
                    escaped = false;
                    return None;
                }
                match ch {
                    '\\' => {
                        escaped = true;
                        None
                    }
                    '"' => Some(index),
                    _ => None,
                }
            })?;
            items.push(serde_json::from_str::<String>(&rest[..=end]).ok()?);
            rest = &rest[end + 1..];
        } else {
            let (item, tail) = rest
                .find(SEPARATOR)
                .map_or((rest, ""), |at| (&rest[..at], &rest[at..]));
            if item.contains('"') {
                return None;
            }
            items.push(item.to_owned());
            rest = tail;
        }
        if rest.is_empty() {
            return Some(items);
        }
        rest = rest.strip_prefix(SEPARATOR)?;
        if rest.is_empty() {
            return None;
        }
    }
}

/// 标签按 `, ` 连接，含 `, ` 或 `"` 的标签写成 JSON 字符串。
fn join(items: &[String]) -> String {
    items
        .iter()
        .map(|item| {
            if item.contains(SEPARATOR) || item.contains('"') {
                serde_json::to_string(item).unwrap_or_else(|_| item.clone())
            } else {
                item.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(SEPARATOR)
}

/// 这道题的答案是否完全落在选项里（结果文本二选一的判断）。
fn within_options(question: &Value, answer: Option<&String>) -> bool {
    let Some(answer) = answer.filter(|answer| !answer.is_empty() && *answer != NOTES_ONLY) else {
        return true;
    };
    let labels = labels(question);
    if labels.contains(&answer.as_str()) {
        return true;
    }
    if question.get("multiSelect").and_then(Value::as_bool) != Some(true) {
        return false;
    }
    split(answer).is_some_and(|items| {
        !items.is_empty()
            && join(&items) == *answer
            && items.iter().all(|item| labels.contains(&item.as_str()))
    })
}

/// 结果文本（没有 annotations / response / followUp 的路径）。
pub(crate) fn result_text(input: &Value, answers: &BTreeMap<String, String>) -> String {
    let questions = questions(input);
    let joined: Vec<String> = questions
        .iter()
        .filter_map(|question| {
            let text = question_text(question);
            answers
                .get(text)
                .filter(|answer| !answer.is_empty() && *answer != NOTES_ONLY)
                .map(|answer| format!("\"{text}\"=\"{answer}\""))
        })
        .collect();
    if joined.is_empty() {
        return texts::NO_ANSWER.to_owned();
    }
    let joined = joined.join(", ");
    if questions
        .iter()
        .all(|question| within_options(question, answers.get(question_text(question))))
    {
        texts::questions_answered(&joined)
    } else {
        texts::user_answered(&joined)
    }
}

#[async_trait]
impl Tool for AskUserQuestionTool {
    fn name(&self) -> &str {
        NAME
    }

    fn input_schema(&self) -> Value {
        self.definition.schema.clone()
    }

    fn description(&self, _context: &PromptContext) -> String {
        self.definition.description.clone()
    }

    fn is_enabled(&self) -> bool {
        self.interactive
    }

    fn search_hint(&self) -> Option<&str> {
        Some("prompt the user with a multiple-choice question")
    }

    fn is_read_only(&self, _input: &Value) -> bool {
        true
    }

    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        true
    }

    fn max_result_size_chars(&self) -> Option<usize> {
        Some(100_000)
    }

    async fn validate_input(&self, input: &Value, _context: &ToolUseContext) -> Validation {
        check(input).map_or(Validation::Ok, invalid)
    }

    async fn check_permissions(
        &self,
        _input: &Value,
        context: &ToolUseContext,
    ) -> PermissionDecision {
        user_interaction(context.mode, texts::ANSWER_QUESTIONS)
    }

    async fn call(
        &self,
        input: Value,
        context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let request = InteractionRequest::Question {
            tool_use: context.tool_use.clone(),
            questions: input.get("questions").cloned().unwrap_or(Value::Null),
        };
        let Ok(answer) = crate::asking::ask::<QuestionAnswer>(&context, request).await else {
            return Err(ToolError::Cancelled);
        };
        Ok(match answer {
            Some(QuestionAnswer::Answered { answers }) => text(result_text(&input, &answers)),
            // esc 或交互没有回答：按权限对话框的拒绝处理。
            Some(QuestionAnswer::Declined) | None => rejected(REJECT_MESSAGE.to_owned()),
        })
    }
}

/// 需要用户亲自回答的工具：交互本身就是询问，判决链放行；
/// dontAsk 下返回询问，由判决链外层按 dontAsk 拒绝（docs/specs/rust-cli/tools-more.md §1）。
pub(crate) fn user_interaction(mode: PermissionMode, message: &str) -> PermissionDecision {
    if mode == PermissionMode::DontAsk {
        return PermissionDecision::Ask {
            message: message.to_owned(),
            suggestions: Vec::new(),
        };
    }
    PermissionDecision::Allow {
        updated_input: None,
    }
}

#[cfg(test)]
#[path = "ask_user_question_tests.rs"]
mod tests;
