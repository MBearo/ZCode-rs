//! Node `input-validation-model-content.ts`: what the model reads when its
//! first call fails the tool schema. Parameter issues (missing, unexpected,
//! wrong type) become sentences; anything else is the issue JSON.
use crate::zod::{Issue, J};

fn received(message: &str) -> &str {
    message
        .split_once("received ")
        .map(|(_, rest)| {
            rest.split(|c: char| !c.is_alphanumeric() && c != '_')
                .next()
                .unwrap_or("")
        })
        .unwrap_or("")
}

/// `<tool_use_error>InputValidationError: …</tool_use_error>` for `issues`.
pub fn render(tool: &str, issues: &[Issue]) -> String {
    let (mut missing, mut unexpected, mut wrong) = (vec![], vec![], vec![]);
    for issue in issues {
        match issue.code() {
            "invalid_type" if issue.message().contains("received undefined") => {
                missing.push(format!(
                    "The required parameter `{}` is missing",
                    issue.path_text()
                ));
            }
            "invalid_type" => {
                let expected = match issue.field("expected") {
                    Some(J::S(expected)) => expected.as_str(),
                    _ => "",
                };
                wrong.push(format!(
                    "The parameter `{}` type is expected as `{expected}` but provided as `{}`",
                    issue.path_text(),
                    received(issue.message())
                ));
            }
            "unrecognized_keys" => {
                let prefix = issue.path_text();
                if let Some(J::A(keys)) = issue.field("keys") {
                    for key in keys {
                        if let J::S(key) = key {
                            let path = if prefix.is_empty() {
                                key.clone()
                            } else {
                                format!("{prefix}.{key}")
                            };
                            unexpected
                                .push(format!("An unexpected parameter `{path}` was provided"));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    let lines: Vec<String> = missing.into_iter().chain(unexpected).chain(wrong).collect();
    let body = if lines.is_empty() {
        J::A(issues.iter().map(Issue::json).collect()).pretty()
    } else {
        let noun = if lines.len() > 1 { "issues" } else { "issue" };
        format!(
            "{tool} failed due to the following {noun}:\n{}",
            lines.join("\n")
        )
    };
    format!("<tool_use_error>InputValidationError: {body}</tool_use_error>")
}
