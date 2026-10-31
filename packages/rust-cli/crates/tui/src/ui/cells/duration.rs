//! 回合结束那一行（docs/specs/rust-cli/tui-display.md §2.7，修复线 F118）：`✻ <动词> for <用时> · done <时刻>`，后台还有
//! 在跑的命令时接 ` · <pill> still running`（pill 由调用方每次画时现取）；等后台子代理时整行换成
//! `Waiting for N background agent(s) to finish`。整行 dim，按宽度折行，续行缩进 2 列。
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use rust_cli_base::units;
use rust_cli_kernel_api::turn_duration::TurnDuration;

use super::{Ctx, dim, duration};
use crate::ui::i18n::Lang;
use crate::ui::text::wrap;

/// 动词（与对齐对象的清单相同）。
const VERBS: [&str; 8] = [
    "Baked",
    "Brewed",
    "Churned",
    "Cogitated",
    "Cooked",
    "Crunched",
    "Sautéed",
    "Worked",
];

/// 字符串哈希的乘数（`h = h·31 + 字符`）。
const STRING_HASH_MULTIPLIER: i32 = 31;
/// 12 小时制半天的小时数。
const HOURS_PER_HALF_DAY: i64 = 12;

/// 按种子的字符串哈希挑动词：`h = h·31 + 字符`（UTF-16 单元，32 位有符号回绕），取无符号后对动词数（8）取余。种子是
/// 回合 ID，同一个回合每次画都一样。
pub(crate) fn verb(seed: &str) -> &'static str {
    let hash = seed.encode_utf16().fold(0_i32, |hash, unit| {
        hash.wrapping_mul(STRING_HASH_MULTIPLIER)
            .wrapping_add(i32::from(unit))
    });
    let index = u32::from_ne_bytes(hash.to_ne_bytes()) as usize % VERBS.len();
    VERBS[index]
}

/// `at_ms` 加上 UTC 偏移之后的时刻：英文 12 小时制（`4:52 AM`），中文 24 小时制（`16:52`），小时不补零。
pub(crate) fn clock(at_ms: u64, offset_minutes: i32, lang: Lang) -> String {
    let minutes =
        i64::try_from(at_ms / units::MS_PER_MINUTE).unwrap_or(i64::MAX) + i64::from(offset_minutes);
    let of_day = minutes.rem_euclid(units::MINUTES_PER_DAY as i64);
    let per_hour = units::MINUTES_PER_HOUR as i64;
    let (hour, minute) = (of_day / per_hour, of_day % per_hour);
    match lang {
        Lang::En => {
            let half = if hour < HOURS_PER_HALF_DAY {
                "AM"
            } else {
                "PM"
            };
            let hour = match hour % HOURS_PER_HALF_DAY {
                0 => HOURS_PER_HALF_DAY,
                hour => hour,
            };
            format!("{hour}:{minute:02} {half}")
        }
        Lang::Zh => format!("{hour}:{minute:02}"),
    }
}

/// 这一行。`still_running` 是 footer 的 pill（没有在跑的命令时为 None）；等后台子代理的行不接它。
pub(crate) fn lines(
    line: &TurnDuration,
    still_running: Option<&str>,
    ctx: &Ctx<'_>,
) -> Vec<Line<'static>> {
    let style = dim();
    let spans = if line.waiting_agents > 0 {
        let count = line.waiting_agents;
        let (before, after) = match ctx.lang {
            Lang::En => (
                "Waiting for ",
                if count == 1 {
                    " background agent to finish"
                } else {
                    " background agents to finish"
                },
            ),
            Lang::Zh => ("等待 ", " 个后台代理完成"),
        };
        vec![
            Span::styled(before, style),
            Span::styled(count.to_string(), style.add_modifier(Modifier::BOLD)),
            Span::styled(after, style),
        ]
    } else {
        let took = duration(line.duration_ms);
        let at = clock(line.finished_at_ms, line.utc_offset_minutes, ctx.lang);
        let mut text = match ctx.lang {
            Lang::En => format!("{} for {took} · done {at}", verb(line.turn.as_str())),
            Lang::Zh => format!("用时 {took} · 完成于 {at}"),
        };
        if let Some(still) = still_running {
            text.push_str(&match ctx.lang {
                Lang::En => format!(" · {still} still running"),
                Lang::Zh => format!(" · {still} 仍在运行"),
            });
        }
        vec![Span::styled(text, style)]
    };
    wrap(
        &spans,
        ctx.width,
        &[Span::styled("✻ ", style)],
        &[Span::raw("  ")],
    )
}

#[cfg(test)]
#[path = "duration_tests.rs"]
mod tests;
