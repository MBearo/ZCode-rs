//! bypass 模式的宣布（`auto_mode` attachment；docs/specs/rust-cli/shell-permissions.md §4.9、context.md §14，
//! 修复线 F43）。
//!
//! 分三步：决定产生、找上一条、渲染。本工程只做 bypass 类别（bypassPermissions 与 guarded，同跨会话的
//! [`ModeClass`]）；auto 下的宣布与离开 auto 的 `auto_mode_exit` 没有做（shell-permissions.md §7）。
//!
//! 两个模型条件按常量处理：本工程总用 lean 提示词与有卡片的模型那一版的写法，bashFirst 的开关为真，steer 是
//! `relaxed`。

use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::peers::ModeClass;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_prompts::texts;
use serde_json::{Value, json};

pub(crate) const SOURCE: &str = "auto_mode";
/// 离开 auto 时的宣布。本工程不产生；找“上一条”时认它。
const EXIT_SOURCE: &str = "auto_mode_exit";

/// bashFirst 要求的工具（有 Bash，且有 Edit 或 Write）。
const BASH: &str = "Bash";
const EDIT: &str = "Edit";
const WRITE: &str = "Write";

/// 这一步要不要宣布 bypass 模式。`history` 是最近一次压缩边界之后的部分，`mode` 是判决用的模式（plan 开时是
/// plan），`tool_names` 是这一步的工具。
///
/// - 模式不是 bypass 类别时不宣布（不是 auto 也不是 bypassPermissions）；
/// - 工具里没有 Bash，或 Edit、Write 都没有时不宣布（bypass 一定是 steerOnly，不是 bashFirst 就不产生）；
/// - 最近一条 `auto_mode` / `auto_mode_exit` 是 `auto_mode` 时不再宣布（steerOnly 的上一条总是挡住）。
pub(crate) fn announce(
    history: &[Message],
    mode: PermissionMode,
    tool_names: &[String],
) -> Option<(String, Value)> {
    if ModeClass::of(mode) != ModeClass::Bypass {
        return None;
    }
    let has = |name: &str| tool_names.iter().any(|tool| tool == name);
    if !(has(BASH) && (has(EDIT) || has(WRITE))) {
        return None;
    }
    let last = history.iter().rev().find_map(|message| match message {
        Message::Attachment(attachment)
            if attachment.source == SOURCE || attachment.source == EXIT_SOURCE =>
        {
            Some(attachment.source.as_str())
        }
        _ => None,
    });
    if last == Some(SOURCE) {
        return None;
    }
    Some((
        texts::auto_mode_bypass().to_owned(),
        json!({
            "autoModeConsentFlow": false,
            "bashFirst": true,
            "bashFirstSteer": "relaxed",
            "steerOnly": true,
            "bypass": true,
        }),
    ))
}

#[cfg(test)]
#[path = "auto_mode_tests.rs"]
mod tests;
