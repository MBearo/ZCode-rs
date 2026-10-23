//! `-p "/context"`（docs/specs/rust-cli/context-usage.md §1、§5.1）：内核算出用量，按非交互的输出格式
//! 写成 markdown。不建会话、不发模型请求。

use std::fmt::Write as _;
use std::sync::Arc;

use anyhow::Result;
use rust_cli_kernel_api::context_usage::{
    ContextUsage, ContextUsageQuery, format_estimate, format_tokens,
};
use rust_cli_kernel_api::session_api::{EndReason, Kernel};

use crate::{PrintOptions, existing_session};

/// 整个 prompt 是 `/context`（可带参数，非交互的命令不看参数）。
pub(crate) fn is_context(prompt: &str) -> bool {
    let prompt = prompt.trim();
    prompt == "/context" || prompt.starts_with("/context ")
}

/// JS `((tokens / window) * 100).toFixed(1)`。
fn percent(tokens: u64, window: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let value = if window == 0 {
        0.0
    } else {
        tokens as f64 / window as f64 * 100.0
    };
    let tenths = (value * 10.0).round();
    format!("{:.1}", tenths / 10.0)
}

/// 非交互的 markdown（外部版本：没有 ant-only 的段落）。
pub fn markdown(usage: &ContextUsage) -> String {
    let window = usage.raw_max_tokens;
    let mut out = String::from("## Context Usage\n\n");
    let _ = writeln!(out, "**Model:** {}  ", usage.model);
    let _ = writeln!(
        out,
        "**Tokens:** {} / {} ({}%)",
        format_tokens(usage.total_tokens),
        format_tokens(window),
        usage.percentage
    );
    if let Some(over) = usage.over_limit() {
        let _ = writeln!(out, "**Over limit:** {over}");
    }
    out.push('\n');
    let row = |out: &mut String, name: &str, tokens: u64| {
        let _ = writeln!(
            out,
            "| {name} | {} | {}% |",
            format_tokens(tokens),
            percent(tokens, window)
        );
    };
    let shown: Vec<_> = usage
        .categories
        .iter()
        .filter(|c| c.tokens > 0 && c.name != "Free space" && c.name != "Autocompact buffer")
        .collect();
    if !shown.is_empty() {
        out.push_str("### Estimated usage by category\n\n");
        out.push_str("| Category | Tokens | Percentage |\n");
        out.push_str("|----------|--------|------------|\n");
        for category in shown {
            row(&mut out, &category.name, category.tokens);
        }
        for name in ["Free space", "Autocompact buffer"] {
            if let Some(found) = usage
                .categories
                .iter()
                .find(|c| c.name == name && c.tokens > 0)
            {
                row(&mut out, name, found.tokens);
            }
        }
        out.push('\n');
    }
    let mut table = |title: &str, header: &str, rows: Vec<String>| {
        if rows.is_empty() {
            return;
        }
        let rule: String = header
            .split('|')
            .filter(|cell| !cell.is_empty())
            .map(|cell| format!("|{}", "-".repeat(cell.len())))
            .collect();
        let _ = write!(out, "### {title}\n\n{header}\n{rule}|\n");
        for line in rows {
            let _ = writeln!(out, "{line}");
        }
        out.push('\n');
    };
    table(
        "MCP Tools",
        "| Tool | Server | Tokens |",
        usage
            .mcp_tools
            .iter()
            .map(|tool| {
                format!(
                    "| {} | {} | {} |",
                    tool.name,
                    tool.server,
                    format_tokens(tool.tokens)
                )
            })
            .collect(),
    );
    table(
        "Custom Agents",
        "| Agent Type | Source | Tokens |",
        usage
            .agents
            .iter()
            .map(|agent| {
                format!(
                    "| {} | {} | {} |",
                    agent.agent_type,
                    agent.source,
                    format_tokens(agent.tokens)
                )
            })
            .collect(),
    );
    table(
        "Memory Files",
        "| Type | Path | Tokens |",
        usage
            .memory_files
            .iter()
            .map(|file| {
                format!(
                    "| {} | {} | {} |",
                    file.kind,
                    file.path,
                    format_tokens(file.tokens)
                )
            })
            .collect(),
    );
    let skills: u64 = usage.skills.iter().map(|skill| skill.tokens).sum();
    if skills > 0 {
        table(
            "Skills",
            "| Skill | Source | Tokens |",
            usage
                .skills
                .iter()
                .map(|skill| {
                    let source = match &skill.plugin {
                        Some(plugin) => format!("{} ({plugin})", skill.source),
                        None => skill.source.clone(),
                    };
                    format!(
                        "| {} | {source} | {} |",
                        skill.name,
                        format_estimate(skill.tokens)
                    )
                })
                .collect(),
        );
    }
    out
}

/// 算出用量的 markdown（以换行结尾）。新会话（没有 `-c` / `-r`）不建会话；续写时打开那个会话、结束时 SessionEnd。
pub(crate) async fn text(kernel: Arc<dyn Kernel>, options: PrintOptions) -> Result<String> {
    // 找不到会话的 `-c` 与新会话相同（session-lifecycle.md §1）。
    let session = existing_session(kernel.as_ref(), &options).await?;
    let query = ContextUsageQuery {
        session: session.as_ref().map(|session| session.id().clone()),
        workspace: options.workspace.clone(),
        selection: options.selection.clone(),
        mode: options.mode,
    };
    let usage = kernel.context_usage(query).await;
    if let Some(session) = &session {
        let _ = session.close(EndReason::Other).await;
    }
    // text 输出原样打印命令结果（已以换行结尾，不再补）。
    Ok(markdown(&usage?))
}

#[cfg(test)]
#[path = "context_tests.rs"]
mod tests;
