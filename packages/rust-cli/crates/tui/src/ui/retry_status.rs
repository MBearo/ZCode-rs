//! 重试与停顿的状态行（docs/specs/rust-cli/stall-visibility.md §3）：`✻ <错误> · Retrying in 5s · attempt 1/10`、
//! `✻ Waiting for API response · will retry in 4m 40s ·
//! check your network`。图标与前半用 error 色，后半 dim。主线程的状态行与前台子代理的 Agent 行共用。
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use rust_cli_kernel_api::model::ModelNotice;
use unicode_width::UnicodeWidthStr;

use super::cells::duration;
use super::i18n::Lang;
use super::text::truncate;
use crate::view::Notice;

/// 状态行的图标。
pub const GLYPH: &str = "✻";

fn error() -> Style {
    Style::default().fg(Color::Red)
}

fn dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

/// 倒计时：向上取整到秒；不少于 5 分钟时只写最大的单位。
pub fn countdown(ms: u64) -> String {
    let ms = ms.div_ceil(1000) * 1000;
    if ms < 300_000 {
        return duration(ms);
    }
    // 天、时、分取整（这里已是整秒，秒不会进位）；只写最大的非零单位。
    let (days, hours, minutes) = (
        ms / 86_400_000,
        (ms % 86_400_000) / 3_600_000,
        (ms % 3_600_000) / 60_000,
    );
    if days > 0 {
        format!("{days}d")
    } else if hours > 0 {
        format!("{hours}h")
    } else {
        format!("{minutes}m")
    }
}

/// 重试中的两半：前两次只写 `API error`，之后写具体的错误；后半是倒计时与第几次。
fn retrying(
    attempt: u32,
    max_retries: u32,
    error: String,
    left: &str,
    lang: Lang,
) -> (String, String) {
    let what = if attempt >= max_retries.min(3) {
        error
    } else {
        match lang {
            Lang::En => "API error".to_owned(),
            Lang::Zh => "API 错误".to_owned(),
        }
    };
    let tail = match lang {
        Lang::En => format!(" · Retrying in {left} · attempt {attempt}/{max_retries}"),
        Lang::Zh => format!(" · {left}后重试 · 第 {attempt}/{max_retries} 次"),
    };
    (what, tail)
}

/// 停住时的两半。
fn stalled(left: &str, lang: Lang) -> (String, String) {
    match lang {
        Lang::En => (
            "Waiting for API response".to_owned(),
            format!(" · will retry in {left} · check your network"),
        ),
        Lang::Zh => (
            "等待 API 响应".to_owned(),
            format!(" · {left}后重试 · 请检查网络"),
        ),
    }
}

/// 状态行的两半：error 色的说明与 dim 的倒计时。`Resumed` 没有状态行。
pub fn parts(notice: &Notice, now_ms: u64, lang: Lang) -> Option<(String, String)> {
    let left = countdown(notice.remaining_ms(now_ms));
    match &notice.notice {
        ModelNotice::Retrying {
            attempt,
            max_retries,
            error,
            ..
        } => Some(retrying(
            *attempt,
            *max_retries,
            error.to_string(),
            &left,
            lang,
        )),
        ModelNotice::Stalled { .. } => Some(stalled(&left, lang)),
        ModelNotice::Resumed => None,
    }
}

/// 两半排成一行（宽 `width`）：说明截断到 `max(10, 宽 − 2 − 后半宽 − 2)`，让倒计时放得下。
fn laid_out((what, tail): (String, String), width: usize) -> Vec<Span<'static>> {
    let room = width.saturating_sub(2 + tail.width() + 2).max(10);
    vec![
        Span::styled(format!("{GLYPH} "), error()),
        Span::styled(truncate(&what, room), error()),
        Span::styled(tail, dim()),
    ]
}

/// 整行（宽 `width`）。
pub fn spans(notice: &Notice, now_ms: u64, lang: Lang, width: usize) -> Vec<Span<'static>> {
    parts(notice, now_ms, lang).map_or_else(Vec::new, |parts| laid_out(parts, width))
}

/// 被查看的子代理的重试与停顿（列表快照的 `activity`，修复线 F112，stall-visibility.md §4.3）：与主线程的重试行同一套
/// 文字；`left_ms` 是快照里的剩余时长减去收到快照后经过的时间。别的活动没有重试行。
pub fn activity_line(
    activity: &rust_cli_kernel_api::panels::SubagentActivity,
    left_ms: u64,
    lang: Lang,
    width: usize,
) -> Option<Line<'static>> {
    use rust_cli_kernel_api::panels::SubagentActivity;
    let left = countdown(left_ms);
    let parts = match activity {
        SubagentActivity::Retrying {
            attempt,
            max_retries,
            error,
            ..
        } => retrying(*attempt, *max_retries, error.clone(), &left, lang),
        SubagentActivity::Stalled { .. } => stalled(&left, lang),
        SubagentActivity::Thinking | SubagentActivity::Tool { .. } => return None,
    };
    Some(super::text::truncate_line(
        Line::from(laid_out(parts, width)),
        width,
    ))
}

/// 主线程的状态行（整行换掉 spinner）。
pub fn line(notice: &Notice, now_ms: u64, lang: Lang, width: usize) -> Line<'static> {
    super::text::truncate_line(Line::from(spans(notice, now_ms, lang, width)), width)
}

#[cfg(test)]
#[path = "retry_status_tests.rs"]
mod tests;
