//! Monitor 的文字（docs/specs/rust-cli/tools-more.md §19）：工具结果、事件与说明、终态通知的 summary、Bash 与 CronCreate
//! 随开关多出的句子、ws 的询问与生命周期。
//!
//! `prompts/monitor/` 下的文件手工维护（来源写在 spec §19 各节），文字逐字固定。

use rust_cli_base::include_lf;

use crate::texts::render;
use crate::trim;

macro_rules! monitor_texts {
    ($($name:ident => $file:literal,)*) => {
        $(
            #[doc = concat!("`prompts/monitor/", $file, ".txt`")]
            fn $name() -> &'static str {
                trim(include_lf!(concat!("../../../prompts/monitor/", $file, ".txt")))
            }
        )*

        #[cfg(test)]
        pub(crate) const ALL: &[(&str, fn() -> &'static str)] = &[$(($file, $name)),*];
    };
}

monitor_texts! {
    started_text => "started",
    event_summary_text => "event_summary",
    expired_silent_text => "expired_silent",
    expired_text => "expired",
    suppressed_text => "suppressed",
    too_much_output_text => "too_much_output",
    ended_text => "ended",
    ended_silent_text => "ended_silent",
    failed_text => "failed",
    stopped_text => "stopped",
    stopped_notice_text => "stopped_notice",
    bash_sentence_text => "bash_sentence",
    bash_blocked_text => "bash_blocked",
    cron_create_section_text => "cron_create_section",
    ws_private_text => "ws_private",
    ws_ask_text => "ws_ask",
    ws_error_text => "ws_error",
    ws_rejected_text => "ws_rejected",
    ws_dropped_text => "ws_dropped",
    ws_closed_text => "ws_closed",
}

/// 通知里的转义（`&`、`<`、`>`），说明与正文都用它。
pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// 工具结果：`duration` 是期限的写法（`5m`、`3s`）。
pub fn started(task: &str, duration: &str) -> String {
    render(started_text(), &[("task", task), ("duration", duration)])
}

/// 事件与说明的 `<task-notification>` 信封（不带说明与包装）：任务 ID、`Monitor event: "<说明>"` 与 `<event>` 正文，都转义。
pub fn event_envelope(task: &str, description: &str, body: &str) -> String {
    format!(
        "<task-notification>\n<task-id>{}</task-id>\n<summary>{}</summary>\n<event>{}</event>\n</task-notification>",
        escape(task),
        event_summary(&escape(description)),
        escape(body),
    )
}

/// 事件通知的 `<summary>`（说明已转义）。
pub fn event_summary(description: &str) -> String {
    render(event_summary_text(), &[("description", description)])
}

/// 到期说明：`events` 是已送出的事件数。
pub fn expired(duration: &str, events: u64) -> String {
    if events == 0 {
        return render(expired_silent_text(), &[("duration", duration)]);
    }
    let count = events.to_string();
    let noun = if events == 1 { "event" } else { "events" };
    render(
        expired_text(),
        &[("duration", duration), ("count", &count), ("noun", noun)],
    )
}

/// 压下之后第一次拿到令牌时的说明。
pub fn suppressed(count: u64) -> String {
    render(suppressed_text(), &[("count", &count.to_string())])
}

/// 连续压下太久、停掉时的说明。
pub fn too_much_output(count: u64, seconds: u64) -> String {
    render(
        too_much_output_text(),
        &[
            ("count", &count.to_string()),
            ("seconds", &seconds.to_string()),
        ],
    )
}

/// 终态通知的结局。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    /// 退出码 0。`output`：stdout 有没有输出。
    Completed {
        output: bool,
    },
    Failed,
    Stopped,
}

/// 终态通知的 `<summary>`（说明已转义）。`exit_code` 只写进“没有输出”与“失败”两种。
pub fn terminal_summary(description: &str, ending: Ending, exit_code: Option<i32>) -> String {
    let exit = exit_code
        .map(|code| format!(" (exit {code})"))
        .unwrap_or_default();
    let template = match ending {
        Ending::Completed { output: true } => ended_text(),
        Ending::Completed { output: false } => ended_silent_text(),
        Ending::Failed => failed_text(),
        Ending::Stopped => stopped_text(),
    };
    render(template, &[("description", description), ("exit", &exit)])
}

/// 用户停掉 ws 来源的 monitor 时的说明。
pub fn stopped_notice() -> &'static str {
    stopped_notice_text()
}

/// Bash 描述里 `run_in_background` 一行末尾多出的一句（前面带空格）。
pub fn bash_sentence() -> &'static str {
    bash_sentence_text()
}

/// Bash 拦前台 `sleep` 的报错：`what` 是 `sleep 30 followed by: …` 或 `standalone sleep 30`。
pub fn bash_blocked(what: &str) -> String {
    render(bash_blocked_text(), &[("what", what)])
}

/// CronCreate 描述在 `## Runtime behavior` 之前多出的一节（不带末尾的空行）。
pub fn cron_create_section() -> &'static str {
    cron_create_section_text()
}

/// ws 地址在私有段时的拒绝。
pub fn ws_private(host: &str) -> String {
    render(ws_private_text(), &[("host", host)])
}

/// ws 的询问。
pub fn ws_ask(url: &str, protocols: &[String]) -> String {
    let protocols = if protocols.is_empty() {
        String::new()
    } else {
        format!(" (subprotocols: {})", protocols.join(", "))
    };
    render(ws_ask_text(), &[("url", url), ("protocols", &protocols)])
}

/// ws 出错的说明。
pub fn ws_error(message: &str) -> String {
    render(ws_error_text(), &[("message", message)])
}

/// ws 升级被拒的说明。
pub fn ws_rejected(status: u16) -> String {
    render(ws_rejected_text(), &[("status", &status.to_string())])
}

/// ws 一帧太大、关闭的说明。
pub fn ws_dropped(bytes: u64) -> String {
    render(ws_dropped_text(), &[("bytes", &bytes.to_string())])
}

/// ws 关闭的说明：有原因时前面加空格。
pub fn ws_closed(code: u16, reason: &str) -> String {
    let reason = if reason.is_empty() {
        String::new()
    } else {
        format!(" {reason}")
    };
    render(
        ws_closed_text(),
        &[("code", &code.to_string()), ("reason", &reason)],
    )
}

#[cfg(test)]
#[path = "monitor_tests.rs"]
mod tests;
