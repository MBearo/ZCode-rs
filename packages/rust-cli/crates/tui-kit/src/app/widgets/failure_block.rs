//! 回合失败的块（docs/puffin-tui.md §4.2「回合失败」「再试一次」；docs/specs/rust-cli/tui-kit.md §4.4）：结束失败回合的块，
//! 三种墨色——从 fg3 落定到 err 的 `✗` 与 fg2 的句子、agent 的原话与次数（事实行）、上限什么时候回来、^O 时轨道上的错误
//! 字段、下一步（命令 fg2，`⏎ tries again`）。措辞只看原因，从不看消息的文字。入参是中性的 [`Failure`] 与 [`Names`]。
//! 移植自 Puffin 的 shell/failures.rs 的块的纯函数部分（Failures.scala 第 1–150 行）；重试行在 `app::frame`。
use crate::app::element::{Element, Wrap, by_width, single_line, stack, wrap_text};
use crate::markdown::render::INDENT;
use crate::render::ansi::RESET;
use crate::render::width::string_width;
use crate::render::{Span, Style};
use crate::theme::theme::Theme;

use super::fade;
use super::fit::measured;

/// 句子里的名字：provider 的短名、它的主机与接入方式（登录还是密钥）、模型。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Names {
    pub provider: String,
    pub host: String,
    pub signs_in: bool,
    pub model: String,
}

/// 一次回合失败的事实（Puffin 投影的 `FailureItem`）：`http_status` 为 0、`tries` 为 0 是没有；`resend` 是有一条可以再发的
/// 消息。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Failure<'a> {
    pub code: &'a str,
    pub message: &'a str,
    /// 认得的原因（`rate_limited`、`auth_failed`…）；不知道时为 ""。
    pub reason: &'a str,
    pub http_status: u16,
    pub provider_id: &'a str,
    pub model_id: &'a str,
    pub error_phase: &'a str,
    pub request_id: &'a str,
    pub retryable: bool,
    /// 这个回合里重试过时是试过的次数，否则 0。
    pub tries: u32,
    pub resend: bool,
    /// 失败的时刻（epoch ms）；不知道时为 0。
    pub at: u64,
}

/// 原因说的是什么：每种一句话、一个下一步。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Auth,
    Limited,
    Capped,
    TooLong,
    NotOffered,
    Trouble,
    Unreachable,
    Proxy,
    Stalled,
    Hook,
    Other,
}

fn kind_of(it: &Failure<'_>) -> Kind {
    if it.code == "fault.runtime.hookBlocked" {
        return Kind::Hook;
    }
    match it.reason {
        "auth_failed" | "provider_not_configured" => Kind::Auth,
        "rate_limited" | "offpeak_queued" => {
            if it.retryable {
                Kind::Limited
            } else {
                Kind::Capped
            }
        }
        "context_exceeded" => Kind::TooLong,
        "invalid_request" if it.http_status == 404 => Kind::NotOffered,
        "server_error" | "provider_overloaded" => Kind::Trouble,
        "network_failed" | "stale_connection" => Kind::Unreachable,
        "proxy_error" | "tls_error" => Kind::Proxy,
        "timeout" | "stream_idle_timeout" => Kind::Stalled,
        _ => Kind::Other,
    }
}

/// 套餐的上限停下了回合：它的块可以说上限什么时候回来（§4.2「回来的时刻」）。
pub fn capped(it: &Failure<'_>) -> bool {
    kind_of(it) == Kind::Capped
}

/// 块里的字：每种原因的句子、下一步里命令与键后面的话、事实行的次数。别的界面语言给译文；铺法不变。
pub struct FailureWords {
    pub sentence: fn(Kind, &Names) -> String,
    /// `⏎` 后面。
    pub tries_again: &'static str,
    /// `/login` 后面。
    pub signs_in_again: &'static str,
    /// `/settings` 后面。
    pub has_the_key: &'static str,
    /// `/model` 后面（上限）；`_then` 是后面还接着 `⏎` 的。
    pub can_switch: &'static str,
    pub can_switch_then: &'static str,
    /// `/compact` 后面；`_then` 是后面还接着 `⏎` 的。
    pub shortens: &'static str,
    pub shortens_then: &'static str,
    /// `/model` 后面（不提供这个模型）。
    pub picks_another: &'static str,
    /// `HTTPS_PROXY` 前后。
    pub behind_proxy: &'static str,
    pub start_again: &'static str,
    /// 失败的回合改过文件时下一步之前。
    pub edits_kept: &'static str,
    pub tried: fn(u32) -> String,
    /// ^O 时错误的字段名：code、status、provider、model、request、at（补到 10 格，按显示宽度）。
    pub fields: [&'static str; 6],
}

/// Puffin 的字。
pub const FAILURE_WORDS: FailureWords = FailureWords {
    sentence: said,
    tries_again: " tries again.",
    signs_in_again: " signs in again.",
    has_the_key: " has the key.",
    can_switch: " can switch to another model.",
    can_switch_then: " can switch to another model. ",
    shortens: " shortens it.",
    shortens_then: " shortens it; then ",
    picks_another: " picks another one.",
    behind_proxy: "Behind a proxy? Set ",
    start_again: " and start ZCode again.",
    edits_kept: "The edits so far are kept. ",
    tried: |n| format!("tried {n} times"),
    fields: ["code", "status", "provider", "model", "request", "at"],
};

/// 发生了什么，平常的话。
pub fn sentence(it: &Failure<'_>, n: &Names) -> String {
    said(kind_of(it), n)
}

/// [`sentence`]，字是 `w` 的。
pub fn sentence_said(it: &Failure<'_>, n: &Names, w: &FailureWords) -> String {
    (w.sentence)(kind_of(it), n)
}

/// Puffin 每种原因的句子。
fn said(kind: Kind, n: &Names) -> String {
    match kind {
        Kind::Auth if n.signs_in => format!("{} did not accept your sign-in.", n.provider),
        Kind::Auth => format!("{} did not accept your API key.", n.provider),
        Kind::Limited => format!("{} is limiting requests.", n.provider),
        Kind::Capped => format!("{} has stopped requests on this plan for now.", n.provider),
        Kind::TooLong => format!("The conversation is too long for {}.", n.model),
        Kind::NotOffered => format!("{} does not offer {} on this plan.", n.provider, n.model),
        Kind::Trouble => format!("{} had a problem on its side.", n.provider),
        Kind::Unreachable | Kind::Proxy => format!("Could not reach {}.", n.host),
        Kind::Stalled => "The answer stopped coming.".to_owned(),
        Kind::Hook => "A hook stopped your message.".to_owned(),
        Kind::Other => "The model request failed.".to_owned(),
    }
}

/// 再试一次回合有用；有可以再发的消息时块说 `⏎ tries again`。
pub fn retries(it: &Failure<'_>) -> bool {
    matches!(
        kind_of(it),
        Kind::Limited
            | Kind::Capped
            | Kind::TooLong
            | Kind::Trouble
            | Kind::Unreachable
            | Kind::Stalled
    )
}

/// ⏎ 可以再发失败回合的消息（§4.2「再试一次」）：原因说可以，有那条消息。
pub fn offers_again(it: &Failure<'_>) -> bool {
    retries(it) && it.resend
}

/// 下一步，作为（文字，是不是命令或键）的片段；没有时为空。`edits` 是失败的回合改过文件。
pub fn next(it: &Failure<'_>, n: &Names, edits: bool) -> Vec<(String, bool)> {
    next_said(it, n, edits, &FAILURE_WORDS)
}

/// [`next`]，字是 `w` 的。
pub fn next_said(
    it: &Failure<'_>,
    n: &Names,
    edits: bool,
    w: &FailureWords,
) -> Vec<(String, bool)> {
    let again = offers_again(it);
    let piece = |t: &str, cmd: bool| (t.to_owned(), cmd);
    let try_again = if again {
        vec![piece("⏎", true), piece(w.tries_again, false)]
    } else {
        Vec::new()
    };
    let step = match kind_of(it) {
        Kind::Auth if n.signs_in => vec![piece("/login", true), piece(w.signs_in_again, false)],
        Kind::Auth => vec![piece("/settings", true), piece(w.has_the_key, false)],
        Kind::Capped => {
            let mut v = vec![
                piece("/model", true),
                piece(
                    if again {
                        w.can_switch_then
                    } else {
                        w.can_switch
                    },
                    false,
                ),
            ];
            v.extend(try_again);
            v
        }
        Kind::TooLong => {
            let mut v = vec![
                piece("/compact", true),
                piece(if again { w.shortens_then } else { w.shortens }, false),
            ];
            v.extend(try_again);
            v
        }
        Kind::NotOffered => vec![piece("/model", true), piece(w.picks_another, false)],
        Kind::Proxy => vec![
            piece(w.behind_proxy, false),
            piece(rust_cli_base::names::env::HTTPS_PROXY, true),
            piece(w.start_again, false),
        ],
        Kind::Limited | Kind::Trouble | Kind::Unreachable | Kind::Stalled => try_again,
        Kind::Hook | Kind::Other => Vec::new(),
    };
    if step.is_empty() || !edits {
        step
    } else {
        std::iter::once(piece(w.edits_kept, false))
            .chain(step)
            .collect()
    }
}

/// 块：`✗` 与句子、agent 的原话、上限什么时候回来（`back`，带它自己的年龄）、^O 时错误的字段、下一步。`age` 是它到达
/// 以来的 tick：记号从 fg3 落定到 err，其余从地升起。`utc_offset` 是 `it.at` 那一刻本地时间相对 UTC 的分钟数（^O 的
/// `at` 行用）。
#[allow(clippy::too_many_arguments)]
pub fn block(
    theme: &Theme,
    it: &Failure<'_>,
    n: &Names,
    edits: bool,
    open: bool,
    age: i64,
    back: Option<(Vec<Span>, i64)>,
    utc_offset: i32,
) -> Element {
    block_said(
        theme,
        it,
        n,
        edits,
        open,
        age,
        back,
        utc_offset,
        &FAILURE_WORDS,
    )
}

/// [`block`]，字是 `w` 的（别的界面语言）。
#[allow(clippy::too_many_arguments)]
pub fn block_said(
    theme: &Theme,
    it: &Failure<'_>,
    n: &Names,
    edits: bool,
    open: bool,
    age: i64,
    back: Option<(Vec<Span>, i64)>,
    utc_offset: i32,
    w: &FailureWords,
) -> Element {
    let ink = |st: Style| -> Style {
        fade::fade_in(theme, vec![Span::new("", st)], age, fade::TICKS, None)
            .first()
            .map_or(st, |s| s.style)
    };
    let mark_ink = fade::ink(
        theme,
        theme.fg3.fg_color(),
        Style::fg(theme.err),
        age,
        fade::TICKS,
    );
    let head = wrap_text(
        format!(" {}✗{RESET} ", mark_ink.set_sequence()),
        INDENT,
        format!(
            "{}{}{RESET}",
            ink(theme.fg2).set_sequence(),
            sentence_said(it, n, w)
        ),
        Wrap::Word,
    );
    let facts = facts_of(it, w);
    let mut rows = vec![head];
    if !facts.is_empty() {
        let line_ink = ink(theme.line);
        let fg3 = ink(theme.fg3);
        rows.push(by_width(move |w| {
            let sep = "  ·  ";
            let joined = facts.join(sep);
            if string_width(&joined) as i64 <= w as i64 - INDENT.len() as i64 {
                let mut spans = vec![Span::new(INDENT, Style::DEFAULT)];
                for (i, f) in facts.iter().enumerate() {
                    if i > 0 {
                        spans.push(Span::new(sep, line_ink));
                    }
                    spans.push(Span::new(f.clone(), fg3));
                }
                single_line(spans, false)
            } else {
                wrap_text(
                    INDENT,
                    INDENT,
                    format!("{}{joined}{RESET}", fg3.set_sequence()),
                    Wrap::Word,
                )
            }
        }));
    }
    // 回来的时刻在事实与下一步之间，从背景色淡入（它比块晚到）。
    if let Some((spans, at)) = back {
        let mut line = vec![Span::new(INDENT, Style::DEFAULT)];
        line.extend(fade::fade_in(theme, spans, at, fade::TICKS, None));
        rows.push(single_line(line, false));
    }
    if open {
        rows.extend(details(theme, it, &ink, utc_offset, &w.fields));
    }
    let step = next_said(it, n, edits, w);
    if !step.is_empty() {
        let words: String = step
            .iter()
            .map(|(t, cmd)| {
                let st = ink(if *cmd { theme.fg2 } else { theme.fg3 });
                format!("{}{t}{RESET}", st.set_sequence())
            })
            .collect();
        rows.push(wrap_text(INDENT, INDENT, words, Wrap::Word));
    }
    measured(stack(rows))
}

/// agent 的原话、HTTP 状态与看到的重试次数。
fn facts_of(it: &Failure<'_>, w: &FailureWords) -> Vec<String> {
    let message = it.message.replace('\n', " ").trim().to_owned();
    let mut out = Vec::new();
    if !message.is_empty() {
        out.push(message);
    }
    if it.http_status > 0 {
        out.push(format!("HTTP {}", it.http_status));
    }
    if it.tries >= 2 {
        out.push((w.tried)(it.tries));
    }
    out
}

/// ^O：轨道上的错误字段，给问题报告用；空的不写。
fn details(
    theme: &Theme,
    it: &Failure<'_>,
    ink: &dyn Fn(Style) -> Style,
    utc_offset: i32,
    fields: &[&'static str; 6],
) -> Vec<Element> {
    let http = if it.http_status > 0 {
        format!("HTTP {}", it.http_status)
    } else {
        String::new()
    };
    let status = [http.as_str(), it.reason, it.error_phase]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    let at = if it.at > 0 {
        clock_of(it.at as f64, utc_offset)
    } else {
        String::new()
    };
    let rows = [
        (fields[0], it.code.to_owned()),
        (fields[1], status),
        (fields[2], it.provider_id.to_owned()),
        (fields[3], it.model_id.to_owned()),
        (fields[4], it.request_id.to_owned()),
        (fields[5], at),
    ];
    rows.into_iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(name, value)| {
            single_line(
                vec![
                    Span::new(INDENT, Style::DEFAULT),
                    Span::new("│", ink(theme.line)),
                    Span::new(" ", Style::DEFAULT),
                    Span::new(
                        format!(
                            "{name}{}",
                            " ".repeat(10usize.saturating_sub(string_width(name)))
                        ),
                        ink(theme.fg3),
                    ),
                    Span::new(value, ink(theme.fg2)),
                ],
                false,
            )
        })
        .collect()
}

/// `HH:MM:SS`，本地时间：epoch 毫秒 `ms` 加上 `utc_offset` 分钟。
pub fn clock_of(ms: f64, utc_offset: i32) -> String {
    let secs = (ms / 1000.0).floor() as i64 + i64::from(utc_offset) * 60;
    let day = secs.rem_euclid(86_400);
    format!("{:02}:{:02}:{:02}", day / 3600, (day % 3600) / 60, day % 60)
}
