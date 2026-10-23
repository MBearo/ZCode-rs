//! `/loop` 与 ScheduleWakeup 用到的文本（`packages/rust-cli/prompts/loop/`，docs/specs/rust-cli/tools-more.md
//! §17）。
//!
//! 文字逐字固定：skill 提示里随环境变化的片段按本工程取定值（没有云端调度、PR Steward、推送通知与 brief 模式，
//! 不预先报告状态；Monitor 的超时随形态，是模板变量 `{{monitor_timeout_ms}}` / `{{monitor_minutes}}`）；另有触发时的
//! 短提醒与自主说明。没有产品名需要替换。
//! 模板变量用 [`crate::texts::render`] 填入。

use rust_cli_base::include_lf;

use crate::trim;

/// 自主默认、固定间隔的哨兵（CronCreate 的 prompt）。
pub const AUTONOMOUS: &str = "<<autonomous-loop>>";
/// 自主默认、动态节奏的哨兵（ScheduleWakeup 的 prompt）。
pub const AUTONOMOUS_DYNAMIC: &str = "<<autonomous-loop-dynamic>>";
/// loop.md、固定间隔的哨兵。
pub const FILE: &str = "<<loop.md>>";
/// loop.md、动态节奏的哨兵。
pub const FILE_DYNAMIC: &str = "<<loop.md-dynamic>>";

macro_rules! texts {
    ($($(#[$doc:meta])* $name:ident => $path:literal,)*) => {
        $(
            $(#[$doc])*
            pub fn $name() -> &'static str {
                trim(include_lf!(concat!("../../../prompts/loop/", $path, ".txt")))
            }
        )*
    };
}

texts! {
    /// 普通提示（参数里有 prompt）：`{{input}}`、`{{monitor_timeout_ms}}`、`{{monitor_minutes}}`。
    prompt => "prompt",
    /// 自主默认、固定间隔：`{{interval}}`、`{{preamble}}`。
    autonomous_fixed => "autonomous_fixed",
    /// 自主默认、动态节奏：`{{preamble}}`、`{{monitor_timeout_ms}}`、`{{monitor_minutes}}`。
    autonomous_dynamic => "autonomous_dynamic",
    /// loop.md、固定间隔：`{{path}}`、`{{interval}}`、`{{content}}`。
    file_fixed => "file_fixed",
    /// loop.md、动态节奏：`{{path}}`、`{{content}}`、`{{monitor_timeout_ms}}`、`{{monitor_minutes}}`。
    file_dynamic => "file_dynamic",
    /// 自主默认的短提醒（固定间隔）。
    tick_fixed => "tick_fixed",
    /// 自主默认的短提醒（动态节奏）。
    tick_dynamic => "tick_dynamic",
    /// loop.md 的短提醒（固定间隔）。
    file_tick_fixed => "file_tick_fixed",
    /// loop.md 的短提醒（动态节奏）。
    file_tick_dynamic => "file_tick_dynamic",
    /// loop.md 不在时的短提醒（动态节奏）。
    file_absent_dynamic => "file_absent_dynamic",
    /// loop.md 内容变了时发的全文：`{{path}}`、`{{content}}`、`{{tick}}`。
    file_delivery => "file_delivery",
}

/// 自主说明。原样返回：末尾的换行是文本的一部分。
pub fn preamble() -> &'static str {
    include_lf!("../../../prompts/loop/preamble.md")
}

/// 内容不同于上次时视为“没发过 loop.md”的标记。
pub const PREAMBLE_DELIVERED: &str = "__autonomous_preamble__";
/// loop.md 的长度上限。
const FILE_LIMIT: usize = 25_000;

/// 读到的 loop.md。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopFile {
    pub path: String,
    pub content: String,
}

/// 去掉首尾空白，空的当作没有；超过 25000 个字符时在这之前最后一个换行处截断并加警告行。
pub fn loop_file(path: &str, raw: &str) -> Option<LoopFile> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let content = if trimmed.chars().count() <= FILE_LIMIT {
        trimmed.to_owned()
    } else {
        let head: String = trimmed.chars().take(FILE_LIMIT + 1).collect();
        let cut = match head.rfind('\n') {
            Some(index) if index > 0 => index,
            _ => head
                .char_indices()
                .nth(FILE_LIMIT)
                .map_or(head.len(), |(index, _)| index),
        };
        format!(
            "{}\n\n> WARNING: loop.md was truncated to {FILE_LIMIT} bytes. Keep the task list concise.",
            &head[..cut]
        )
    };
    Some(LoopFile {
        path: path.to_owned(),
        content,
    })
}

/// loop.md 的两个位置：项目根下的 `.zcode/loop.md`，再是 `~/.zcode/loop.md`。`join` 是执行环境的路径拼法
/// （调用方传 kernel-api 的 `PathStyle::join`）。
///
/// 修复线 F58：以前这里自己按路径里有没有 `\` 选分隔符、只去末尾的分隔符，与规则文件、子代理、skill 的
/// `PathStyle::join` 结果不同（home 里有 `..`、`//`，或 Windows 上写成 `C:/…` 时）。
pub fn loop_file_paths(
    root: &str,
    home: Option<&str>,
    join: impl Fn(&str, &[&str]) -> String,
) -> Vec<String> {
    let file = |base: &str| join(base, &[rust_cli_base::names::DOT_DIR, "loop.md"]);
    std::iter::once(file(root)).chain(home.map(file)).collect()
}

#[cfg(test)]
#[path = "loops_tests.rs"]
mod tests;
