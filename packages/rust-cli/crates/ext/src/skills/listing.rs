//! skill 清单的行与预算（docs/specs/rust-cli/ext.md §2.3）。

use std::sync::Arc;

use rust_cli_kernel_api::listing::ListingEntry;

use super::Skill;
use super::names::valid_display;

/// 描述的最大字符数（缺省 1536）。
const MAX_DESCRIPTION_CHARS: usize = 1536;
/// 没有模型卡片时的上下文窗口。
const DEFAULT_CONTEXT_TOKENS: u64 = 200_000;
/// 清单占窗口的比例（`skillListingBudgetFraction` 缺省 0.01）。
const BUDGET_PERCENT: u64 = 1;

/// 清单的字符预算：窗口 × 每 token 字符数 × 0.01。每 token 字符数按模型
/// （`rust_cli_prompts::models::chars_per_token`）：修 bug（F10，context-usage.md D5）：以前一律按 4，
/// 除 14 个旧模型以外（claude-opus-5-5、GLM 等）应按 3。
pub(crate) fn budget(context_window: Option<u64>, chars_per_token: u64) -> usize {
    let tokens = context_window.unwrap_or(DEFAULT_CONTEXT_TOKENS);
    let chars = tokens * chars_per_token * BUDGET_PERCENT / 100;
    usize::try_from(chars.max(1)).unwrap_or(usize::MAX)
}

/// `description`，有 `when_to_use` 时接 ` - <when_to_use>`，超长时截断加 `…`。
fn description(skill: &Skill) -> String {
    let text = match &skill.when_to_use {
        Some(when) => format!("{} - {when}", skill.description),
        None => skill.description.clone(),
    };
    if text.chars().count() > MAX_DESCRIPTION_CHARS {
        let mut cut: String = text.chars().take(MAX_DESCRIPTION_CHARS - 1).collect();
        cut.push('…');
        cut
    } else {
        text
    }
}

/// 清单里的名字：显示名与名字不同、合法、且只指向这个 skill 时写作 `名字 (显示名)`。
fn label(skill: &Skill, all: &[Arc<Skill>]) -> String {
    let Some(display) = skill.display.as_deref() else {
        return skill.name.clone();
    };
    let unique = !all.iter().any(|other| {
        other.name != skill.name
            && (other.name == display || other.display.as_deref() == Some(display))
    });
    if display == skill.name
        || skill.name.ends_with(&format!(":{display}"))
        || !valid_display(display)
        || !unique
    {
        skill.name.clone()
    } else {
        format!("{} ({display})", skill.name)
    }
}

/// 宽度按字符数，不按终端显示宽度（ext.md §8 D4 的近似）。
fn width(text: &str) -> usize {
    text.chars().count()
}

/// 清单项：`visible` 是要宣布的 skill（已按顺序），`all` 是全部 skill（判断显示名是否唯一）。
pub(crate) fn entries(
    visible: &[Arc<Skill>],
    all: &[Arc<Skill>],
    limit: usize,
) -> Vec<ListingEntry> {
    let rows: Vec<(String, String)> = visible
        .iter()
        .map(|skill| {
            let name_only = format!("- {}", label(skill, all));
            let full = format!("{name_only}: {}", description(skill));
            (name_only, full)
        })
        .collect();
    let total: usize =
        rows.iter().map(|(_, full)| width(full)).sum::<usize>() + rows.len().saturating_sub(1);
    let lines: Vec<String> = if total <= limit {
        rows.iter().map(|(_, full)| full.clone()).collect()
    } else {
        // 超出预算：先只算名字，再按清单顺序把完整描述放回（本工程没有使用频率，不按频率排序）。
        tracing::warn!(
            skills = rows.len(),
            total,
            limit,
            "skill listing over budget; descriptions are dropped"
        );
        let mut left = limit.saturating_sub(
            rows.iter().map(|(name, _)| width(name)).sum::<usize>() + rows.len().saturating_sub(1),
        );
        rows.iter()
            .map(|(name, full)| {
                let extra = width(full) - width(name);
                if extra <= left {
                    left -= extra;
                    full.clone()
                } else {
                    name.clone()
                }
            })
            .collect()
    };
    visible
        .iter()
        .zip(lines)
        .map(|(skill, line)| ListingEntry {
            name: skill.name.clone(),
            line,
        })
        .collect()
}

#[cfg(test)]
#[path = "listing_tests.rs"]
mod tests;
