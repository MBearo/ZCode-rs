//! AskUserQuestion（docs/specs/rust-cli/tools-more.md §1）。
//!
//! 提问本身就是这个工具的权限询问；本工程把它表达成 `InteractionRequest::Question`，
//! 判决链直接放行，在 `call` 里问用户、按回答拼出结果文本。

use std::collections::{BTreeMap, HashSet};

use async_trait::async_trait;
use rust_cli_kernel_api::orchestration::{InteractionRequest, QuestionAnnotation, QuestionAnswer};
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
/// 每个问题的选项数与问题数的范围（tools-more.md 的 AskUserQuestion）。
const MIN_OPTIONS: usize = 2;
const MAX_OPTIONS: usize = 4;
const MAX_QUESTIONS: usize = 4;

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
    if questions
        .iter()
        .any(|question| labels(question).len() < MIN_OPTIONS)
    {
        // 引导语排在校验错误之前。
        return Some(format!(
            "InputValidationError: {}\n\nquestions: every question needs 2-4 options",
            texts::SINGLE_OPTION_STEER
        ));
    }
    if questions.is_empty() || questions.len() > MAX_QUESTIONS {
        return Some("InputValidationError: questions: expected 1-4 questions".to_owned());
    }
    if questions
        .iter()
        .any(|question| labels(question).len() > MAX_OPTIONS)
    {
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

/// 这道题的答案是否完全落在选项里（结果文本二选一的判断）；有备注的题不算。
fn within_options(question: &Value, answer: Option<&String>, notes: bool) -> bool {
    if notes {
        return false;
    }
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

/// 用户的回答（`QuestionAnswer::Answered` 的各项，docs/specs/rust-cli/tools-more.md §1.1）。
pub(crate) struct Answers<'a> {
    pub answers: &'a BTreeMap<String, String>,
    pub annotations: &'a BTreeMap<String, QuestionAnnotation>,
    pub response: Option<&'a str>,
    pub follow_up: bool,
    pub afk_timeout_ms: Option<u64>,
}

/// 一道题在结果里的写法：有答案或备注时 `"<问题>"="<答案>"`（没有答案时 `=(no option selected)`），后接预览与备注。
fn described(question: &Value, given: &Answers<'_>) -> Option<String> {
    let text = question_text(question);
    let answer = given
        .answers
        .get(text)
        .filter(|answer| !answer.is_empty() && *answer != NOTES_ONLY);
    let annotation = given.annotations.get(text);
    let notes = annotation
        .and_then(|annotation| annotation.notes.as_deref())
        .filter(|notes| !notes.is_empty());
    if answer.is_none() && notes.is_none() {
        return None;
    }
    let mut parts = vec![match answer {
        Some(answer) => format!("\"{text}\"=\"{answer}\""),
        None => format!("\"{text}\"=(no option selected)"),
    }];
    if let Some(preview) = annotation
        .and_then(|annotation| annotation.preview.as_deref())
        .filter(|preview| !preview.is_empty())
    {
        parts.push(format!("selected preview:\n{preview}"));
    }
    if let Some(notes) = notes {
        parts.push(format!("notes: {notes}"));
    }
    Some(parts.join(" "))
}

/// 结果文本：空闲超时、再多问几个、直接回复、逐题的答案，都没有时“没有回答”（修复线 F72 以前只有后两种）。
pub(crate) fn result_text(input: &Value, given: &Answers<'_>) -> String {
    let questions = questions(input);
    let joined: Vec<String> = questions
        .iter()
        .filter_map(|question| described(question, given))
        .collect();
    let joined = (!joined.is_empty()).then(|| joined.join(", "));
    let response = given
        .response
        .filter(|response| !response.trim().is_empty());
    if let Some(timeout) = given.afk_timeout_ms.filter(|timeout| *timeout > 0) {
        return match &joined {
            Some(joined) => texts::idle_selected(timeout, joined),
            None => texts::idle_timeout(timeout),
        };
    }
    if given.follow_up {
        let also = response.map_or_else(String::new, |response| {
            format!(" They also wrote: \"{response}\".")
        });
        return texts::more_questions(joined.as_deref(), &also);
    }
    if let Some(response) = response {
        return texts::user_responded(response);
    }
    let Some(joined) = joined else {
        return texts::NO_ANSWER.to_owned();
    };
    let has_notes = |question: &Value| {
        given
            .annotations
            .get(question_text(question))
            .and_then(|annotation| annotation.notes.as_deref())
            .is_some_and(|notes| !notes.is_empty())
    };
    if questions.iter().all(|question| {
        within_options(
            question,
            given.answers.get(question_text(question)),
            has_notes(question),
        )
    }) {
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
        Some(rust_cli_kernel_api::tool::DEFAULT_MAX_RESULT_CHARS)
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
            Some(QuestionAnswer::Answered {
                answers,
                annotations,
                response,
                follow_up,
                afk_timeout_ms,
            }) => text(result_text(
                &input,
                &Answers {
                    answers: &answers,
                    annotations: &annotations,
                    response: response.as_deref(),
                    follow_up,
                    afk_timeout_ms,
                },
            )),
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
