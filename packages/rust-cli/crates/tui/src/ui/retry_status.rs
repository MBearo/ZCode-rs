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

/// 状态行的两半：error 色的说明与 dim 的倒计时。`Resumed` 没有状态行。
pub fn parts(notice: &Notice, now_ms: u64, lang: Lang) -> Option<(String, String)> {
    let left = countdown(notice.remaining_ms(now_ms));
    match &notice.notice {
        ModelNotice::Retrying {
            attempt,
            max_retries,
            error,
            ..
        } => {
            // 前两次只写 `API error`，之后写具体的错误。
            let what = if *attempt >= (*max_retries).min(3) {
                error.to_string()
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
            Some((what, tail))
        }
        ModelNotice::Stalled { .. } => Some(match lang {
            Lang::En => (
                "Waiting for API response".to_owned(),
                format!(" · will retry in {left} · check your network"),
            ),
            Lang::Zh => (
                "等待 API 响应".to_owned(),
                format!(" · {left}后重试 · 请检查网络"),
            ),
        }),
        ModelNotice::Resumed => None,
    }
}

/// 整行（宽 `width`）：说明截断到 `max(10, 宽 − 2 − 后半宽 − 2)`，让倒计时放得下。
pub fn spans(notice: &Notice, now_ms: u64, lang: Lang, width: usize) -> Vec<Span<'static>> {
    let Some((what, tail)) = parts(notice, now_ms, lang) else {
        return Vec::new();
    };
    let room = width.saturating_sub(2 + tail.width() + 2).max(10);
    vec![
        Span::styled(format!("{GLYPH} "), error()),
        Span::styled(truncate(&what, room), error()),
        Span::styled(tail, dim()),
    ]
}

/// 主线程的状态行（整行换掉 spinner）。
pub fn line(notice: &Notice, now_ms: u64, lang: Lang, width: usize) -> Line<'static> {
    super::text::truncate_line(Line::from(spans(notice, now_ms, lang, width)), width)
}

#[cfg(test)]
#[path = "retry_status_tests.rs"]
mod tests;
