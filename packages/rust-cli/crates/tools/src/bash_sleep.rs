//! Monitor 开着时 Bash 拦前台的长 `sleep`（docs/specs/rust-cli/tools-more.md §19.9）。

use std::sync::LazyLock;

use regex::Regex;
use rust_cli_base::js::json::js_double;
use rust_cli_base::js::string::trim;

/// 拦下的最短秒数。
const BLOCK_FROM: f64 = 25.0;

static SLEEP: LazyLock<Regex> = LazyLock::new(|| {
    #[expect(clippy::expect_used, reason = "固定的正则")]
    Regex::new(r"^sleep\s+(\d+(?:\.\d*)?)\s*$").expect("valid sleep pattern")
});

/// 命令拆成的子命令原文：拆不出时是整条命令。
fn command_texts(command: &str) -> Vec<String> {
    if command.is_empty() {
        return Vec::new();
    }
    let analysis = rust_cli_policy::analyze(command);
    if analysis.has_parse_errors {
        return vec![command.to_owned()];
    }
    analysis
        .commands
        .into_iter()
        .map(|invocation| invocation.command_text)
        .collect()
}

/// 第一个子命令整个是 `sleep <秒数>` 且不少于 25 秒时，返回拦下的说明（`sleep 30 followed by: …` 或
/// `standalone sleep 30`）。
pub(crate) fn blocked(command: &str) -> Option<String> {
    let texts = command_texts(command);
    let first = trim(texts.first()?);
    let seconds: f64 = SLEEP.captures(first)?.get(1)?.as_str().parse().ok()?;
    if seconds < BLOCK_FROM {
        return None;
    }
    let shown = js_double(seconds);
    let rest = texts[1..].join(" ");
    let rest = trim(&rest);
    Some(if rest.is_empty() {
        format!("standalone sleep {shown}")
    } else {
        format!("sleep {shown} followed by: {rest}")
    })
}

#[cfg(test)]
#[path = "bash_sleep_tests.rs"]
mod tests;
