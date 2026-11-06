//! 待处理交互 → kit 的请求（docs/specs/rust-cli/tui-kit.md §4.2「权限询问、提问、计划审批」）：旧界面的请求照 Puffin 的
//! 投影整理成 kit 的 [`Ask`]，板上的行、行下面的预览都由 kit 的 `widgets::asks` 画；回答再换回内核的回答。
//! 规则照 Puffin 的 host/fold/asks.rs（`ask_of`、`preview_of`、`pages_of`、`options_of`、`answer`）与
//! host/fold/tools.rs（`display_name`、`summary_of`）。旧界面不能依赖 Puffin 的 crate（frontend 不依赖 frontend），
//! 这里是同一规则的重写。
//!
//! 旧界面多出的：Bash 的“不再询问”可以改前缀（工作线 F72，shell-permissions.md §4.10）；改过的前缀是 Always allow 记住的
//! 规则（[`always_rules`]）。
use std::collections::BTreeMap;

use rust_cli_kernel_api::ids::InteractionId;
use rust_cli_kernel_api::orchestration::{
    Answer, InteractionRequest, PermissionAnswer, PlanApprovalAnswer, QuestionAnnotation,
    QuestionAnswer,
};
use rust_cli_kernel_api::permission::{PermissionMode, PermissionRule};
use rust_cli_tui_kit::app::widgets::asks::data::{
    Ask, AskBody, AskField, AskOption, AskOptionKind, AskOrigin, AskPreview, DiffLine,
    QuestionOption, QuestionPage,
};
use rust_cli_tui_kit::app::widgets::tools::Sign;
use serde_json::Value;

use crate::view::Transcript;

pub const ALLOW_ONCE: &str = "allowOnce";
pub const ALLOW_ALWAYS: &str = "allowAlways";
pub const FULL_ACCESS: &str = "fullAccess";
pub const DENY: &str = "deny";
/// 模型在推荐的选项后面写的标记（不区分大小写）。
const RECOMMENDED: &str = "(recommended)";
/// 摘要的最大字数（超出时以 `…` 结尾）。
const SUMMARY_MAX_CHARS: usize = 120;
/// 摘要按次序取输入里第一个有内容的字段。
const SUMMARY_INPUT_KEYS: [&str; 8] = [
    "description",
    "command",
    "file_path",
    "path",
    "pattern",
    "url",
    "query",
    "prompt",
];
/// 启动子代理的工具。
const SUBAGENT_TOOLS: [&str; 2] = ["Agent", "Task"];

fn clip(line: &str) -> String {
    let one = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > SUMMARY_MAX_CHARS {
        let head: String = one.chars().take(SUMMARY_MAX_CHARS - 1).collect();
        format!("{head}…")
    } else {
        one
    }
}

/// 工具行的名字：问你事情的工具按它问的事命名。
pub fn display_name(tool: &str) -> String {
    match tool {
        "AskUserQuestion" | "McpElicitation" => "Question".to_owned(),
        "ExitPlanMode" => "Plan".to_owned(),
        other => other.to_owned(),
    }
}

/// 工具行的摘要（预览里与行已经说了的相同的那一句不再写）。
pub fn summary_of(tool: &str, input: &Value) -> String {
    let text = |key: &str| input[key].as_str().unwrap_or_default();
    match tool {
        "AskUserQuestion" => match input["questions"].as_array().map_or(0, Vec::len) {
            0 => String::new(),
            1 => "1 question".to_owned(),
            n => format!("{n} questions"),
        },
        "ExitPlanMode" => String::new(),
        "Skill" => clip(text("skill")),
        "Bash" => clip(if text("command").is_empty() {
            text("description")
        } else {
            text("command")
        }),
        _ => SUMMARY_INPUT_KEYS
            .iter()
            .map(|key| text(key))
            .find(|value| !value.trim().is_empty())
            .map(clip)
            .unwrap_or_default(),
    }
}

/// 规则的原文：`Tool(content)` 或 `Tool`。
pub fn rule_text(rule: &PermissionRule) -> String {
    match &rule.content {
        Some(content) => format!("{}({content})", rule.tool),
        None => rule.tool.clone(),
    }
}

/// Edit：去掉新旧文本首尾相同的行，剩下删掉的与加上的。
fn edit_lines(old: &str, new: &str) -> Vec<DiffLine> {
    let a: Vec<&str> = old.split('\n').collect();
    let b: Vec<&str> = new.split('\n').collect();
    let head = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let tail = a[head..]
        .iter()
        .rev()
        .zip(b[head..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let line = |sign, text: &&str| DiffLine {
        sign,
        text: (*text).to_owned(),
    };
    a[head..a.len() - tail]
        .iter()
        .map(|text| line(Sign::Remove, text))
        .chain(
            b[head..b.len() - tail]
                .iter()
                .map(|text| line(Sign::Add, text)),
        )
        .collect()
}

/// ApplyPatch：`-` 开头是删掉、`+` 开头是加上（`---`、`+++` 文件头除外），其余原样。
fn patch_lines(patch: &str) -> Vec<DiffLine> {
    patch
        .split('\n')
        .map(|line| {
            let (sign, text) = match line {
                line if line.starts_with('-') && !line.starts_with("---") => {
                    (Sign::Remove, &line[1..])
                }
                line if line.starts_with('+') && !line.starts_with("+++") => {
                    (Sign::Add, &line[1..])
                }
                line => (Sign::Context, line),
            };
            DiffLine {
                sign,
                text: text.to_owned(),
            }
        })
        .collect()
}

fn field_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Number(_) | Value::Bool(_) => value.to_string(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// 权限请求要做的事，按工具整理（docs/puffin-tui.md §4.2「预览」）。
pub fn preview_of(tool: &str, input: &Value) -> AskPreview {
    let text = |key: &str| input[key].as_str().unwrap_or_default().to_owned();
    match tool {
        "Bash" => AskPreview::Command {
            command: text("command"),
            reason: text("description").trim().to_owned(),
        },
        "Edit" => AskPreview::Diff {
            path: text("file_path"),
            lines: edit_lines(&text("old_string"), &text("new_string")),
        },
        "MultiEdit" => AskPreview::Diff {
            path: text("file_path"),
            lines: input["edits"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|edit| {
                    edit_lines(
                        edit["old_string"].as_str().unwrap_or_default(),
                        edit["new_string"].as_str().unwrap_or_default(),
                    )
                })
                .collect(),
        },
        "ApplyPatch" => AskPreview::Diff {
            path: String::new(),
            lines: patch_lines(&text("patch_text")),
        },
        "Write" => AskPreview::File {
            path: text("file_path"),
            text: text("content"),
        },
        "WebFetch" => AskPreview::Fetch {
            url: text("url"),
            prompt: text("prompt"),
        },
        _ => AskPreview::Fields {
            fields: input
                .as_object()
                .into_iter()
                .flatten()
                .filter(|(_, value)| !value.is_null() && value.as_str() != Some(""))
                .map(|(key, value)| AskField {
                    key: key.clone(),
                    value: field_value(value),
                })
                .collect(),
        },
    }
}

/// 去掉末尾的 `(Recommended)`（不区分大小写）；返回去掉后的标签与有没有它。
fn strip_recommended(label: &str) -> (String, bool) {
    let trimmed = label.trim_end();
    let cut = trimmed.len().checked_sub(RECOMMENDED.len());
    match cut.and_then(|cut| Some((cut, trimmed.get(cut..)?))) {
        Some((cut, tail)) if tail.eq_ignore_ascii_case(RECOMMENDED) => {
            (trimmed[..cut].trim_end().to_owned(), true)
        }
        _ => (label.to_owned(), false),
    }
}

/// 提问的页（tools-more.md §1.1 的 `questions`）。
pub fn pages_of(questions: &Value) -> Vec<QuestionPage> {
    questions
        .as_array()
        .into_iter()
        .flatten()
        .map(|question| {
            let text =
                |value: &Value, key: &str| value[key].as_str().unwrap_or_default().to_owned();
            QuestionPage {
                header: text(question, "header"),
                question: text(question, "question"),
                multi: question["multiSelect"].as_bool().unwrap_or(false),
                options: question["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|option| {
                        let value = text(option, "label");
                        let (label, recommended) = strip_recommended(&value);
                        QuestionOption {
                            value,
                            label,
                            description: text(option, "description"),
                            preview: text(option, "preview"),
                            recommended,
                        }
                    })
                    .collect(),
            }
        })
        .collect()
}

/// Always allow 记住的规则：Bash 改过前缀时是前缀得出的规则（`seed`），否则是全部建议规则。
pub fn always_rules(suggestions: &[PermissionRule], seed: Option<&str>) -> Vec<PermissionRule> {
    match seed {
        Some(seed) => super::interaction::remember::rules(seed),
        None => suggestions.to_vec(),
    }
}

fn options_of(suggestions: &[PermissionRule], seed: Option<&str>) -> Vec<AskOption> {
    let option = |id: &str, kind, rules| AskOption {
        id: id.to_owned(),
        kind,
        rules,
    };
    let mut options = vec![option(ALLOW_ONCE, AskOptionKind::AllowOnce, Vec::new())];
    if !suggestions.is_empty() {
        options.push(option(
            ALLOW_ALWAYS,
            AskOptionKind::AllowAlways,
            always_rules(suggestions, seed)
                .iter()
                .map(rule_text)
                .collect(),
        ));
    }
    options.push(option(FULL_ACCESS, AskOptionKind::FullAccess, Vec::new()));
    options.push(option(DENY, AskOptionKind::Deny, Vec::new()));
    options
}

/// 有这个工具行时它的 id，否则 ""（子代理的请求、旧会话的询问）。
fn item_of(view: &Transcript, tool_use: &str) -> String {
    let shown = view
        .rows()
        .iter()
        .filter_map(|row| row.tool())
        .any(|tool| tool.id.as_str() == tool_use);
    if shown {
        tool_use.to_owned()
    } else {
        String::new()
    }
}

/// 子代理的请求：谁在问、启动它的（还在运行的）Agent 调用。
fn origin_of(view: &Transcript, asked_by: Option<&String>) -> Option<AskOrigin> {
    let who = asked_by?;
    let parent = view
        .rows()
        .iter()
        .rev()
        .filter_map(|row| row.tool())
        .find(|tool| tool.status.is_live() && SUBAGENT_TOOLS.contains(&tool.name.as_str()))
        .map(|tool| tool.id.as_str().to_owned())
        .unwrap_or_default();
    Some(AskOrigin {
        who: who.clone(),
        parent_item_id: parent,
    })
}

/// 一个待处理的交互整理成 kit 的请求。`seed`：Bash 的“不再询问”改过的前缀。
pub fn ask_of(
    view: &Transcript,
    id: &InteractionId,
    request: &InteractionRequest,
    seed: Option<&str>,
) -> Ask {
    let tool_of = |id: &str| {
        view.rows()
            .iter()
            .rev()
            .filter_map(|row| row.tool())
            .find(|tool| tool.id.as_str() == id)
            .map(|tool| (tool.name.clone(), tool.input.clone()))
    };
    match request {
        InteractionRequest::Permission {
            tool_use,
            tool,
            input,
            suggestions,
            asked_by,
            machine,
            ..
        } => Ask {
            id: id.to_string(),
            item_id: item_of(view, tool_use.as_str()),
            tool_name: tool.clone(),
            name: display_name(tool),
            summary: summary_of(tool, input),
            origin: origin_of(view, asked_by.as_ref()),
            // kit 只认额外机器的名字：主 env 不标（审查 K9 起询问里的机器是 `MachineName`）。
            machine: machine.remote().map(str::to_owned),
            body: AskBody::Permission {
                preview: preview_of(tool, input),
                options: options_of(suggestions, seed),
            },
        },
        InteractionRequest::Question {
            tool_use,
            questions,
        } => {
            let (tool, input) = tool_of(tool_use.as_str())
                .unwrap_or_else(|| ("AskUserQuestion".to_owned(), Value::Null));
            Ask {
                id: id.to_string(),
                item_id: item_of(view, tool_use.as_str()),
                name: display_name(&tool),
                summary: summary_of(&tool, &input),
                tool_name: tool,
                origin: None,
                machine: None,
                body: AskBody::Question {
                    pages: pages_of(questions),
                    snoozable: false,
                    deadline_at: 0,
                },
            }
        }
        InteractionRequest::PlanApproval { tool_use, plan } => Ask {
            id: id.to_string(),
            item_id: item_of(view, tool_use.as_str()),
            tool_name: "ExitPlanMode".to_owned(),
            name: display_name("ExitPlanMode"),
            summary: String::new(),
            origin: None,
            machine: None,
            body: AskBody::Plan { plan: plan.clone() },
        },
    }
}

/// 权限请求的选项 `option` 换成内核的回答（`seed` 同 [`ask_of`]）；不认识的选项为 None。
pub fn option_answer(
    request: &InteractionRequest,
    option: &str,
    free_text: Option<&str>,
    seed: Option<&str>,
) -> Option<Answer> {
    let InteractionRequest::Permission { suggestions, .. } = request else {
        return None;
    };
    Some(Answer::Permission(match option {
        ALLOW_ONCE | FULL_ACCESS => PermissionAnswer::Allow {
            remember: Vec::new(),
        },
        ALLOW_ALWAYS => PermissionAnswer::Allow {
            remember: always_rules(suggestions, seed),
        },
        DENY => {
            let feedback = free_text
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_owned);
            PermissionAnswer::Deny { feedback }
        }
        _ => return None,
    }))
}

/// 计划的回答：没有字是批准（权限模式不变，修复线 F20），有字是继续计划。
pub fn plan_answer(mode: PermissionMode, feedback: &str) -> Answer {
    let feedback = feedback.trim();
    Answer::PlanApproval(if feedback.is_empty() {
        PlanApprovalAnswer::Approve { mode }
    } else {
        PlanApprovalAnswer::KeepPlanning {
            feedback: Some(feedback.to_owned()),
        }
    })
}

/// 提问的回答：问题文字 → 选中的值（多选用 `, ` 连接）；选中的选项带预览时另带 `annotations`，`notes`（问题文字 →
/// 备注）只在已有 `annotations` 时并进去。全部为空且不是空闲到期时是不回答（Declined）。
pub fn question_answer(
    questions: &[QuestionPage],
    pages: &[Vec<String>],
    notes: Vec<(String, String)>,
    afk_timeout_ms: Option<u64>,
) -> Answer {
    if afk_timeout_ms.is_none() && pages.iter().all(Vec::is_empty) {
        return Answer::Question(QuestionAnswer::Declined);
    }
    let mut answers = BTreeMap::new();
    let mut annotations = BTreeMap::new();
    for (question, values) in questions.iter().zip(pages) {
        if values.is_empty() {
            continue;
        }
        answers.insert(question.question.clone(), values.join(", "));
        let preview = values.first().and_then(|value| {
            question
                .options
                .iter()
                .find(|option| option.value == *value && !option.preview.is_empty())
                .map(|option| option.preview.clone())
        });
        if preview.is_some() {
            annotations.insert(
                question.question.clone(),
                QuestionAnnotation {
                    preview,
                    notes: None,
                },
            );
        }
    }
    // 备注只在已有注记时写进去（同以前写进回答 JSON 的 `annotations` 时：没有这个键就不写）。
    if !annotations.is_empty() {
        for (question, note) in notes {
            annotations.entry(question).or_default().notes = Some(note);
        }
    }
    Answer::Question(QuestionAnswer::Answered {
        answers,
        annotations,
        response: None,
        follow_up: false,
        afk_timeout_ms,
    })
}
