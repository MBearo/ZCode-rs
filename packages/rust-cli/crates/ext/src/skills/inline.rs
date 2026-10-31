//! skill 正文里的 `!` 内联命令（docs/specs/rust-cli/ext.md §2.5）。
//!
//! 命令经同一个注册表里的 Bash 工具执行；权限用判决链，skill 的 `allowed-tools` 作为这一次判决的
//! 额外 allow 规则（放进 `alwaysAllowRules.command`），不询问用户。

use std::sync::{Arc, OnceLock};

use futures_util::future::join_all;
use regex::Regex;
use rust_cli_kernel_api::clock::Ids;
use rust_cli_kernel_api::env::OperationContext;
use rust_cli_kernel_api::message::ToolResultContent;
use rust_cli_kernel_api::permission::{PermissionDecision, PermissionRule};
use rust_cli_kernel_api::tool::{DISCARDED_PROGRESS_CAPACITY, Tool, ToolUseContext};
use rust_cli_policy::PolicyGate;
use serde_json::json;
use tokio::sync::mpsc;

/// 一条 `!` 命令：原文（替换时找它）、命令、出现位置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Inline {
    pub raw: String,
    pub command: String,
}

struct Patterns {
    block: Regex,
    inline: Regex,
    code_span: Regex,
}

/// 编译失败时（不会发生）不识别任何命令。
fn patterns() -> Option<&'static Patterns> {
    static PATTERNS: OnceLock<Option<Patterns>> = OnceLock::new();
    PATTERNS
        .get_or_init(|| {
            Some(Patterns {
                block: Regex::new(r"(?s)```!\s*\n?(.*?)\n?```").ok()?,
                // 规则是后行断言 `(?<=^|\s)`；这里把前导空白放进匹配，命令原文取第 1 组。
                inline: Regex::new(r"(?m)(?:^|\s)(!`([^`]+)`)").ok()?,
                code_span: Regex::new(r"`[^`\n]+`").ok()?,
            })
        })
        .as_ref()
}

/// 不以 `!` 或反引号开头的行内代码换成等长的空白，里面的 `` !` `` 不算命令。
fn mask_code_spans(code_span: &Regex, text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for found in code_span.find_iter(text) {
        let previous = text[..found.start()].chars().next_back();
        out.push_str(&text[last..found.start()]);
        if matches!(previous, Some('!' | '`')) {
            out.push_str(found.as_str());
        } else {
            // 去掉两边的反引号。
            let inner = found.as_str().len() - "``".len();
            out.push('`');
            out.push_str(&" ".repeat(inner));
            out.push('`');
        }
        last = found.end();
    }
    out.push_str(&text[last..]);
    out
}

fn clean(command: &str) -> String {
    command.trim().replace("\r\n", "\n")
}

/// 正文里的 `!` 命令：先块形式，再行内形式。
pub(crate) fn find(text: &str) -> Vec<Inline> {
    let mut found = Vec::new();
    let Some(patterns) = patterns() else {
        return found;
    };
    for capture in patterns.block.captures_iter(text) {
        let command = clean(capture.get(1).map_or("", |inner| inner.as_str()));
        if let (false, Some(raw)) = (command.is_empty(), capture.get(0)) {
            found.push(Inline {
                raw: raw.as_str().to_owned(),
                command,
            });
        }
    }
    if text.contains("!`") {
        // 遮盖只把行内代码的内容换成等长空白，位置不变：原文从未遮盖的文本里取。
        let masked = mask_code_spans(&patterns.code_span, text);
        for capture in patterns.inline.captures_iter(&masked) {
            let (Some(raw), Some(inner)) = (capture.get(1), capture.get(2)) else {
                continue;
            };
            let command = clean(inner.as_str());
            if !command.is_empty() {
                found.push(Inline {
                    raw: text[raw.start()..raw.end()].to_owned(),
                    command,
                });
            }
        }
    }
    found
}

/// 执行 `!` 命令要用到的能力（由 runtime 注入）。
#[derive(Clone)]
pub(crate) struct Runner {
    pub bash: Arc<dyn Tool>,
    pub gate: Arc<PolicyGate>,
    pub ids: Arc<dyn Ids>,
}

fn text_of(content: &[ToolResultContent]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            ToolResultContent::Text { text } => Some(text.as_str()),
            ToolResultContent::Image { .. }
            | ToolResultContent::Document { .. }
            | ToolResultContent::ToolReference { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

impl Runner {
    /// 一条命令的输出；失败时是错误文本。
    async fn run(
        &self,
        inline: &Inline,
        allowed: &[PermissionRule],
        context: &ToolUseContext,
    ) -> Result<String, String> {
        let input = json!({"command": inline.command});
        // 每条命令一个新的 operationId（与 hooks 相同，不单独登记意图）。
        let operation = OperationContext {
            operation: self.ids.operation(),
            ..context.operation.clone()
        };
        let context = ToolUseContext {
            operation,
            ..context.clone()
        };
        let decision = self
            .gate
            .decide_with_allowed(self.bash.as_ref(), &input, &context, allowed)
            .await;
        if let PermissionDecision::Deny { message } | PermissionDecision::Ask { message, .. } =
            &decision
        {
            return Err(format!(
                "Shell command permission check failed for pattern \"{}\": {message}",
                inline.raw
            ));
        }
        if decision == PermissionDecision::Passthrough {
            return Err(format!(
                "Shell command permission check failed for pattern \"{}\": Permission denied",
                inline.raw
            ));
        }
        let (progress, _ignored) = mpsc::channel(DISCARDED_PROGRESS_CAPACITY);
        match self.bash.call(input, context, progress).await {
            Ok(output) if !output.is_error => Ok(text_of(&output.content)),
            Ok(output) => Err(format!(
                "Shell command failed for pattern \"{}\": {}",
                inline.raw,
                text_of(&output.content)
            )),
            Err(error) => Err(format!("[Error]\n{error}")),
        }
    }

    /// 执行全部命令（并发），把原文换成输出（替换时不解释 `$&` 一类的模式）。
    pub(crate) async fn expand(
        &self,
        text: String,
        allowed: &[String],
        context: &ToolUseContext,
    ) -> Result<String, String> {
        let commands = find(&text);
        if commands.is_empty() {
            return Ok(text);
        }
        let rules: Vec<PermissionRule> = allowed
            .iter()
            .map(|rule| rust_cli_policy::rules::parse_rule(rule))
            .collect();
        let outputs = join_all(
            commands
                .iter()
                .map(|inline| self.run(inline, &rules, context)),
        )
        .await;
        let mut text = text;
        for (inline, output) in commands.iter().zip(outputs) {
            let output = output?;
            text = text.replacen(&inline.raw, &output, 1);
        }
        Ok(text)
    }
}

#[cfg(test)]
#[path = "inline_tests.rs"]
mod tests;
