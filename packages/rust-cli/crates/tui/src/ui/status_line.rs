// 改写自 apps/zcode-host/crates/tui/src/ui/status_line.rs（feat/rs 79b6691c55）：回合、字数与推理来自
// `view::Transcript`；标题没有任务清单（L7），用每个回合固定的动词。
//! 运行中的状态行（docs/specs/rust-tui.md §8.3）：`图标 标题… (用时 · ↓ N tokens · 思考状态)`。图标是点阵
//! spinner（docs/specs/rust-cli/tui-display.md §2.1.2）；标题是进行中的任务，没有时用每个回合固定选的一个动词。运行中 10 秒
//! 没有新帧时，图标与标题在接下来的 10 秒里渐渐变红。
use std::time::Duration;

use super::app::App;
use super::i18n::{self, Lang};
use super::palette::{ColorLevel, blend};
use super::text::truncate_line;
use crate::view::Body;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// 绘制用的时间：墙上时钟、动画时间、离上一个会话帧多久。
pub struct Clock {
    pub now_ms: u64,
    pub animation: Duration,
    pub since_frame: Duration,
}

/// 加载动画的帧（用户决定 2026-10-22，tui-display.md §2.1.2 / D24）：点阵 spinner 顺序循环。以前在
/// `· ✢ ✳ ✶ ✻ ✽` 之间来回变化，用户不要“小星星”。
const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// 每帧 80 毫秒，一圈 800 毫秒（重绘间隔 33 毫秒，`bottom::ANIMATION`，不会跳帧）。
const STEP_MS: u128 = 80;
/// 多久没有新帧算卡住，以及变红用的时间。
const STALL: Duration = Duration::from_secs(10);
const FADE: Duration = Duration::from_secs(10);
const STALL_RED: (u8, u8, u8) = (0xe0, 0x50, 0x50);

pub(super) const VERBS_EN: [&str; 30] = [
    "Working",
    "Thinking",
    "Reading",
    "Planning",
    "Building",
    "Tinkering",
    "Crafting",
    "Weaving",
    "Brewing",
    "Sketching",
    "Untangling",
    "Assembling",
    "Shaping",
    "Polishing",
    "Exploring",
    "Mulling",
    "Sorting",
    "Tracing",
    "Composing",
    "Wiring",
    "Forging",
    "Tuning",
    "Juggling",
    "Puzzling",
    "Drafting",
    "Cooking",
    "Stitching",
    "Sifting",
    "Hatching",
    "Calibrating",
];
const VERBS_ZH: [&str; 30] = [
    "处理中",
    "思考中",
    "琢磨中",
    "构思中",
    "整理中",
    "推敲中",
    "编织中",
    "打磨中",
    "梳理中",
    "探索中",
    "拼装中",
    "酝酿中",
    "调试中",
    "推演中",
    "规划中",
    "斟酌中",
    "排查中",
    "组装中",
    "钻研中",
    "演算中",
    "设计中",
    "编排中",
    "筹划中",
    "雕琢中",
    "盘算中",
    "校准中",
    "研读中",
    "绘制中",
    "调配中",
    "搭建中",
];

/// 加载动画当前的帧：按动画时间顺序循环。
pub fn glyph(animation: Duration) -> &'static str {
    let index = (animation.as_millis() / STEP_MS) % FRAMES.len() as u128;
    FRAMES[index as usize]
}

/// 每个回合固定的动词：按回合开始的时间选。
fn verb(lang: Lang, started_ms: u64) -> &'static str {
    let index = (started_ms / 1000) as usize % VERBS_EN.len();
    match lang {
        Lang::En => VERBS_EN[index],
        Lang::Zh => VERBS_ZH[index],
    }
}

/// 本回合回复与推理的字数除以 4。
fn tokens(app: &App) -> usize {
    app.view.streamed_chars() / 4
}

fn tokens_label(count: usize) -> String {
    if count >= 1000 {
        format!("{:.1}k", count as f64 / 1000.0)
    } else {
        count.to_string()
    }
}

/// 推理的状态：进行中按时长分四档；结束后 2 秒内写用了多久。只看本回合的推理。
fn thinking(app: &App, turn_started: u64, now_ms: u64) -> Option<String> {
    let row =
        app.view.rows().iter().rev().find(|row| {
            row.created_ms >= turn_started && matches!(row.body, Body::Reasoning { .. })
        })?;
    let Body::Reasoning {
        streaming,
        duration_ms,
        ..
    } = row.body
    else {
        return None;
    };
    let started = row.created_ms;
    let zh = app.lang == Lang::Zh;
    if streaming {
        let seconds = now_ms.saturating_sub(started) / 1000;
        let label = match (seconds, zh) {
            (0..10, false) => "thinking",
            (10..20, false) => "still thinking",
            (20..45, false) => "thinking more",
            (_, false) => "deep in thought",
            (0..10, true) => "思考中",
            (10..20, true) => "仍在思考",
            (20..45, true) => "深入思考",
            (_, true) => "沉思中",
        };
        return Some(label.to_owned());
    }
    let duration = duration_ms?;
    let ended = started + duration;
    (now_ms.saturating_sub(ended) < 2_000).then(|| {
        let seconds = duration.div_ceil(1000);
        if zh {
            format!("思考了 {seconds}s")
        } else {
            format!("thought for {seconds}s")
        }
    })
}

/// 强调色；卡住时渐渐变红（16 / 256 色终端过半后直接用红色）。
fn color(app: &App, since_frame: Duration) -> Color {
    let accent = app.palette.accent;
    let Some(stalled) = since_frame.checked_sub(STALL) else {
        return accent;
    };
    let t = (stalled.as_secs_f64() / FADE.as_secs_f64()).min(1.0);
    match (app.palette.level, accent) {
        (ColorLevel::TrueColor, Color::Rgb(r, g, b)) => {
            let (r, g, b) = blend((r, g, b), STALL_RED, t);
            Color::Rgb(r, g, b)
        }
        _ if t >= 0.5 => Color::Red,
        _ => accent,
    }
}

/// 状态行（运行中）；其余时候为空。主线程的模型请求在等重试或停住时，整行换成重试行
/// （docs/specs/rust-cli/stall-visibility.md §3）。
pub fn status(app: &App, width: usize, clock: &Clock) -> Vec<Line<'static>> {
    let Some(started) = app.view.turn_started_ms() else {
        return Vec::new();
    };
    if let Some(notice) = app.view.notice() {
        return vec![super::retry_status::line(
            notice,
            clock.now_ms,
            app.lang,
            width,
        )];
    }
    let dim = Style::default().add_modifier(Modifier::DIM);
    let color = color(app, clock.since_frame);
    let mut spans = vec![
        Span::styled(glyph(clock.animation), Style::default().fg(color)),
        Span::raw(" "),
    ];
    let title = format!("{}…", verb(app.lang, started));
    spans.extend(super::shimmer::spans(
        &title,
        clock.animation,
        &app.palette,
        color,
    ));
    let seconds = clock.now_ms.saturating_sub(started) / 1000;
    let mut parts = vec![
        i18n::elapsed_plain(seconds),
        format!("↓ {} tokens", tokens_label(tokens(app))),
    ];
    parts.extend(thinking(app, started, clock.now_ms));
    spans.push(Span::styled(format!(" ({})", parts.join(" · ")), dim));
    vec![truncate_line(Line::from(spans), width)]
}

#[cfg(test)]
#[path = "status_line_tests.rs"]
mod tests;
