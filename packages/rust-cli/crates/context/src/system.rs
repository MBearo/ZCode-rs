//! system prompt 段落与分块、缓存断点。
//!
//! 第一方形状分三块（billing 头不发送，见 context.md §7）：
//! 1. 身份句，无 cache_control；
//! 2. 静态段（lean body），`cache_control {ephemeral, scope: global}`；
//! 3. 会话段，各段用空行连接，`cache_control {ephemeral}`。
//!
//! 只实现 lean 变体：当前模型族都走 lean，非 lean 的完整段落
//! （intro / system / doing tasks / actions / tools / tone）未覆盖，登记在 context.md §8。

use rust_cli_kernel_api::model::{CacheControl, SystemBlock};
use rust_cli_kernel_api::orchestration::ContextRequest;
use rust_cli_prompts::models;
use rust_cli_prompts::texts::{self, render};

use crate::config::{OutputStyle, Surface};
use crate::render::{Bullet, bullets};

/// 提示词里提到的工具名。
pub(crate) const SKILL_TOOL: &str = "Skill";
pub(crate) const AGENT_TOOL: &str = "Agent";
pub(crate) const BASH_TOOL: &str = "Bash";

/// `<total_tokens>` 的预算（padded-countdown 模式的默认值）。
pub(crate) const TOTAL_TOKENS_BUDGET: u64 = 15_000_000;

/// 组装 system prompt 所需的事实。
pub(crate) struct PromptFacts<'a> {
    pub surface: Surface,
    /// 模型接受对话中间的 system 消息（生效配置，答不出时按卡片回退：[`facts_system_turns`]，model.md §11）。
    pub system_turns: bool,
    pub tool_names: &'a [String],
    /// 这一步有可宣布的 skill。
    pub has_skills: bool,
    pub memory_dir: Option<&'a str>,
    pub output_style: Option<&'a OutputStyle>,
    /// `--system-prompt` 的文字（cli-args.md §4）：有它时缺省段落不出现。
    pub custom: Option<&'a str>,
    /// `--append-system-prompt` 的文字。
    pub append: Option<&'a str>,
}

fn cache(scope: Option<&str>) -> Option<CacheControl> {
    Some(CacheControl {
        kind: "ephemeral".to_owned(),
        ttl: None,
        scope: scope.map(str::to_owned),
    })
}

/// 这一步模型是否接受对话中间的 system 消息：请求带的生效配置（修复线 F23），答不出时按卡片（model.md §11）。
/// 与 query 的 normalize 同一个判定，提醒的措辞与消息的形状不会不一致。
pub(crate) fn facts_system_turns(request: &ContextRequest) -> bool {
    models::mid_conversation_system(
        &request.selection.model,
        request
            .capabilities
            .map(|found| found.supports_mid_conversation_system),
    )
}

/// 模型支持对话中间的 system 消息时说 system turn，否则说 `<system-reminder>` 标签
/// （lean 变体）。
fn reminders(system_turns: bool) -> &'static str {
    if system_turns {
        texts::reminders_system_turns()
    } else {
        texts::reminders_tags()
    }
}

/// 静态段：所有会话共用、可跨会话缓存（`scope: global`）。
fn lean_body(facts: &PromptFacts<'_>) -> String {
    let intro = if facts.output_style.is_some() {
        texts::intro_output_style()
    } else {
        texts::intro()
    };
    render(
        texts::lean_body(),
        &[
            ("intro", intro),
            ("security", texts::security()),
            ("reminders", reminders(facts.system_turns)),
        ],
    )
}

/// `# Session-specific guidance`（lean 时不提 Agent 工具的用法）。
fn guidance(facts: &PromptFacts<'_>) -> Option<String> {
    let mut lines = Vec::new();
    if facts.surface == Surface::Interactive {
        lines.push(Bullet::Line(texts::guidance_shell().to_owned()));
    }
    if facts.has_skills && facts.tool_names.iter().any(|name| name == SKILL_TOOL) {
        lines.push(Bullet::Line(texts::guidance_skills().to_owned()));
    }
    if lines.is_empty() {
        return None;
    }
    Some(render(texts::guidance(), &[("lines", &bullets(lines))]))
}

/// 会话段：按固定的顺序，空段跳过。
fn session_sections(facts: &PromptFacts<'_>) -> Vec<String> {
    let mut sections = vec![
        texts::communication_lean().to_owned(),
        texts::pronouns().to_owned(),
        texts::action_caution().to_owned(),
    ];
    sections.extend(guidance(facts));
    if let Some(dir) = facts.memory_dir {
        sections.push(render(texts::memory_lean(), &[("memory_dir", dir)]));
    }
    // 没有环境段（修复线 F51，context.md §6）。`/context` 的计数用同一个列表。
    sections.push(texts::context_management().to_owned());
    sections.push(texts::act_dont_rederive().to_owned());
    sections.push(total_tokens(TOTAL_TOKENS_BUDGET));
    sections
}

pub(crate) fn total_tokens(left: u64) -> String {
    render(texts::total_tokens(), &[("tokens", &left.to_string())])
}

/// `/context` 计数用的段落（分析用的段落列表，context-usage.md §3）：静态段一段，加会话段的各段；
/// 拼起来与 [`system_blocks`] 的后两块相同，不含身份句。给了 `--system-prompt` 时是它，`--append-system-prompt`
/// 是最后一段。
pub(crate) fn sections(facts: &PromptFacts<'_>) -> Vec<String> {
    let mut sections = match facts.custom {
        Some(custom) => vec![custom.to_owned()],
        None => {
            let mut sections = vec![lean_body(facts)];
            sections.extend(session_sections(facts));
            sections
        }
    };
    sections.extend(facts.append.map(str::to_owned));
    sections
}

/// system 段（cli-args.md §4.2，段落的数组分块）：
/// - 缺省：身份句（无 cache_control）、静态段（`scope: global`）、会话段；`--append-system-prompt` 是会话段的最后一段；
/// - `--system-prompt`：身份句与一块“自定义文字 + 追加文字”，两块都是 `{ephemeral}`（没有动态边界，不分静态段）。
pub(crate) fn system_blocks(facts: &PromptFacts<'_>) -> Vec<SystemBlock> {
    if let Some(custom) = facts.custom {
        let text = std::iter::once(custom)
            .chain(facts.append)
            .collect::<Vec<_>>()
            .join("\n\n");
        return vec![
            SystemBlock {
                text: texts::identity().to_owned(),
                cache_control: cache(None),
            },
            SystemBlock {
                text,
                cache_control: cache(None),
            },
        ];
    }
    let mut session = session_sections(facts);
    session.extend(facts.append.map(str::to_owned));
    vec![
        SystemBlock {
            text: texts::identity().to_owned(),
            cache_control: None,
        },
        SystemBlock {
            text: lean_body(facts),
            cache_control: cache(Some("global")),
        },
        SystemBlock {
            text: session.join("\n\n"),
            cache_control: cache(None),
        },
    ]
}

#[cfg(test)]
#[path = "system_tests.rs"]
mod tests;
