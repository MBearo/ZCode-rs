//! Bash 工具的进程级设置与它们渲染出的描述、schema（docs/specs/rust-cli/shell-permissions.md §1.1，工作线 F132 的 T12）。
//!
//! 设置是一个值：runtime 在进程启动时算一次，模型的 Bash 与 bash 模式（tui-input.md §2.2）用同一份。描述与 schema
//! 从数据文件的文字（缺省设置下的写法）出发一次算出，每处改动的锚点在数据文件里恰好出现一次（单测检查）。
//! 以前是 4 个先后有关的 builder 做 8 次替换，锚点找不到时静默不改；bash 模式另建工具、只配了超时。

use serde_json::Value;

use crate::bash_timeouts::BashTimeouts;
use rust_cli_prompts::Definition;

/// “不要用 cat”一行的开头（只在没有去掉这一行时出现；两种点名写法开头相同）。
pub(crate) const AVOID_LINE: &str = "\n- IMPORTANT: Avoid using this tool to run ";
/// Monitor 开着时，这句之后接 Monitor 的一句（tools-more.md §19.9）。
pub(crate) const MONITOR_ANCHOR: &str = "No `&` needed.";
/// schema 里两段带数字的说明的位置。
pub(crate) const TIMEOUT_POINTER: &str = "/properties/timeout/description";
pub(crate) const BACKGROUND_POINTER: &str = "/properties/run_in_background/description";

/// Bash 工具的进程级设置（整个进程不变，工具定义在会话中途不变，缓存前缀不断）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BashSettings {
    /// 前台与后台的缺省与上限（`BASH_*_TIMEOUT_MS`，cli-args.md §9.3；stream-json 输入时后台缺省另算，
    /// [`BashTimeouts::with_streaming_input`]，headless.md §6.1）。
    pub timeouts: BashTimeouts,
    /// 后台命令有没有期限：`-p` 有（缺省），交互会话没有（修复线 F9）。没有时描述与 schema 去掉期限的说明。
    pub background_deadline: bool,
    /// 进程启动时是 bypass 类别：描述去掉 `- IMPORTANT: Avoid using this tool to run …` 一行（修复线 F43，§4.9）。
    pub bash_first_trimmed: bool,
    /// Monitor 开着（工作线 F83，tools-more.md §19.9）：`run_in_background` 一行末尾多一句，前台的长 `sleep` 拦下。
    pub monitor: bool,
}

impl Default for BashSettings {
    /// 单次 `-p` 的写法（S0、B* 基线）：缺省超时、后台有期限、不去掉一行、Monitor 关。
    fn default() -> Self {
        Self {
            timeouts: BashTimeouts::default(),
            background_deadline: true,
            bash_first_trimmed: false,
            monitor: false,
        }
    }
}

impl BashSettings {
    /// 按这份设置渲染描述与 schema。数据文件的文字是缺省超时、后台有期限的写法；换掉带数字的句子、按期限去掉
    /// 期限的说明、按需要去掉一行、加 Monitor 的一句。
    pub(crate) fn render(&self) -> Definition {
        let base = BashTimeouts::default();
        let timeouts = &self.timeouts;
        let mut definition = rust_cli_prompts::definition(crate::bash::NAME).clone();
        let deadline = |text: &str, old: String, new: String| {
            text.replacen(&old, if self.background_deadline { &new } else { "" }, 1)
        };
        let mut description = definition.description.replacen(
            &base.foreground_line(),
            &timeouts.foreground_line(),
            1,
        );
        description = deadline(
            &description,
            base.deadline_sentence(),
            timeouts.deadline_sentence(),
        );
        if self.bash_first_trimmed {
            description = without_line(&description, AVOID_LINE);
        }
        if self.monitor {
            description = description.replacen(
                MONITOR_ANCHOR,
                &format!(
                    "{MONITOR_ANCHOR}{}",
                    rust_cli_prompts::monitor::bash_sentence()
                ),
                1,
            );
        }
        definition.description = description;
        let schema = &mut definition.schema;
        if let Some(Value::String(text)) = schema.pointer_mut(TIMEOUT_POINTER) {
            *text = text.replacen(&base.timeout_parameter(), &timeouts.timeout_parameter(), 1);
        }
        if let Some(Value::String(text)) = schema.pointer_mut(BACKGROUND_POINTER) {
            *text = deadline(
                text,
                base.deadline_parameter(),
                timeouts.deadline_parameter(),
            );
        }
        definition
    }
}

/// 去掉以 `start`（含前面的换行）开头的那一行；没有这一行时原样返回。
fn without_line(text: &str, start: &str) -> String {
    let Some(at) = text.find(start) else {
        return text.to_owned();
    };
    let end = text[at + 1..]
        .find('\n')
        .map_or(text.len(), |offset| at + 1 + offset);
    format!("{}{}", &text[..at], &text[end..])
}

#[cfg(test)]
#[path = "bash_settings_tests.rs"]
mod tests;
