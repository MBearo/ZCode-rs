//! footer 右侧的请求统计（docs/specs/rust-cli/tui-display.md §2.6，修复线 F27，用户决定）：TPS、ctx、缓存命中率。
//! 值只来自内核——命中率是用量表的查询结果（runner 按序号写入 [`FooterStats`]），ctx 与 TPS 是会话 actor 给的上下文
//! 用量与输出速度（`view::Transcript::context_usage`、`output_speed`）；这里只排文字，没有值的项不出现。
//! 修复线 F105：TPS 原来也取用量表（最近一行），只有工具调用的请求没有值；现在取 actor 在内存里记的速度。
use ratatui::text::Span;
use rust_cli_kernel_api::compaction::ContextOccupancy;
use rust_cli_kernel_api::metering::RequestStats;

use super::app::App;
use super::i18n::Lang;

/// runner 写入的最后一份请求统计。回执带请求序号：切换会话与 `/clear` 时 [`Self::clear`] 让在途的回执作废，不用超时。
#[derive(Debug, Default)]
pub struct FooterStats {
    seq: u64,
    stats: RequestStats,
}

impl FooterStats {
    /// 发一次新查询：序号加一并返回。
    pub fn begin(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    /// 查询的回执：序号是最新的才收下（返回是否收下）；查询失败（存储不支持）时当作没有值。
    pub fn arrived(&mut self, seq: u64, stats: Option<RequestStats>) -> bool {
        if seq != self.seq {
            return false;
        }
        self.stats = stats.unwrap_or_default();
        true
    }

    /// 离开会话：清空，在途的回执作废。
    pub fn clear(&mut self) {
        self.seq += 1;
        self.stats = RequestStats::default();
    }

    pub fn stats(&self) -> &RequestStats {
        &self.stats
    }
}

/// 三项的文字（显示顺序：TPS、ctx、cache）；没有值的是 None。
pub struct Items {
    pub tps: Option<Span<'static>>,
    pub ctx: Option<Span<'static>>,
    pub cache: Option<Span<'static>>,
}

/// 四舍五入成整数（值都是有限的非负数）。
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn whole(value: f64) -> u64 {
    value.round().max(0.0) as u64
}

/// ctx 与 cache 的百分比：四舍五入到一位小数，整数也写 `.0`（工作线 F115，用户决定 2026-10-29，tui-display.md §2.6）。
fn tenths(percent: f64) -> String {
    format!("{:.1}", (percent * 10.0).round().max(0.0) / 10.0)
}

/// 上下文占有效窗口的百分比 `min(100, token / W · 100)`；窗口为 0 时是 100。kernel-api 的 `percent()` 是整数的
/// 式子，留给 `N% context used` 的提示用。
#[allow(clippy::cast_precision_loss)]
fn context_percent(usage: ContextOccupancy) -> f64 {
    if usage.max_tokens == 0 {
        return 100.0;
    }
    (usage.used_tokens as f64 / usage.max_tokens as f64 * 100.0).min(100.0)
}

/// 查看子代理时 TPS 与 ctx 取它的只读视图，命中率的回执由 runner 查它的子会话（修复线 F112，tui-panels.md §1.7）；子代理
/// 没有的项不出现，不用主会话的值顶替。
pub fn items(app: &App) -> Items {
    let stats = app.request_stats.stats();
    let shown = app.shown();
    let style = app.palette.secondary();
    let span = |text: String| Span::styled(text, style);
    // `tok/s` 是单位，中英文相同。
    let tps = shown
        .output_speed()
        .and_then(|speed| speed.tokens_per_second())
        .map(|tps| span(format!("{} tok/s", whole(tps))));
    let ctx = shown.context_usage().map(|usage| {
        let percent = tenths(context_percent(usage));
        span(match app.lang {
            Lang::En => format!("ctx {percent}%"),
            Lang::Zh => format!("上下文 {percent}%"),
        })
    });
    let cache = stats.cache_hit_rate().map(|rate| {
        let percent = tenths(rate * 100.0);
        span(match app.lang {
            Lang::En => format!("cache {percent}%"),
            Lang::Zh => format!("缓存 {percent}%"),
        })
    });
    Items { tps, ctx, cache }
}
