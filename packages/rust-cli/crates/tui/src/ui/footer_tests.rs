use ratatui::style::{Color, Modifier};
use rust_cli_kernel_api::compaction::ContextNotice;
use rust_cli_kernel_api::metering::{OutputSpeed, RequestStats};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{KernelEvent, SequencedEvent};
use rust_cli_kernel_api::testing::selection;

use super::*;
use crate::ui::palette::{ColorLevel, Palette};
use crate::view::Transcript;

fn app(lang: Lang, notice: ContextNotice) -> App {
    let mut app = App::new(
        Palette::new(ColorLevel::TrueColor, Some((0, 0, 0))),
        lang,
        Transcript::new(
            selection(),
            PermissionMode::Default,
            crate::view::test_workspace(),
        ),
        Some("/Users/dev".into()),
        "0.1.0".into(),
    );
    app.view.apply(
        SequencedEvent {
            seq: 1,
            revision: 0,
            event: KernelEvent::ContextNotice { notice },
        },
        0,
    );
    app
}

/// 右侧最后一段：上下文提示的文字与样式。
/// 上下文提示：模型名贴最右（F116），它前面是分隔点，再前面是提示。
fn context(app: &App) -> (String, Style) {
    let (line, _) = footer(app, 160);
    let notice = line.spans.iter().rev().nth(2).unwrap();
    (notice.content.to_string(), notice.style)
}

/// F14：上下文快满的四种显示（compact.md §2.1）。真实内核的显示见 tests/suite/context_low.rs；`N% context used`
/// 只在卡片认识、窗口小于 1M 的模型上出现（现在的卡片里没有），`Context low` 要关掉自动压缩（入口的环境变量），
/// 所以这两种在这里按事件核对文字与颜色。
#[test]
fn the_context_notice_has_four_wordings() {
    for (notice, en, zh) in [
        (
            ContextNotice::Used { percent: 82 },
            "82% context used",
            "上下文已用 82%",
        ),
        (
            ContextNotice::UntilAutoCompact { percent: 10 },
            "10% until auto-compact",
            "距自动压缩还剩 10%",
        ),
        (
            ContextNotice::Low {
                percent: 11,
                compact_hint: true,
            },
            "Context low (11% remaining) \u{b7} Run /compact to compact & continue",
            "上下文不足（剩余 11%） \u{b7} 运行 /compact 压缩后继续",
        ),
        (
            ContextNotice::Low {
                percent: 11,
                compact_hint: false,
            },
            "Context low (11% remaining)",
            "上下文不足（剩余 11%）",
        ),
    ] {
        let (text, style) = context(&app(Lang::En, notice));
        assert_eq!(text, en);
        assert_eq!(context(&app(Lang::Zh, notice)).0, zh);
        if matches!(notice, ContextNotice::Low { .. }) {
            // 自动压缩关闭时是错误色。
            assert_eq!(style.fg, Some(Color::Red), "{text}");
        } else {
            assert!(style.add_modifier.contains(Modifier::DIM), "{text}");
        }
    }
    let (line, _) = footer(&app(Lang::En, ContextNotice::Hidden), 160);
    let plain: String = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    assert!(
        !plain.contains("context") && !plain.contains("auto-compact"),
        "{plain}"
    );
}

/// F27 的 App：深色或浅色背景（前景色已知，次要文字是真彩色）、内核的上下文用量与一份请求统计的回执；
/// 有回执时另有会话 actor 给的输出速度（F105 起 TPS 的来源，[`node_speed`]）。
fn stats_app(lang: Lang, light: bool, stats: Option<RequestStats>) -> App {
    let speed = stats.is_some().then(node_speed);
    speed_app(lang, light, stats, speed)
}

fn speed_app(
    lang: Lang,
    light: bool,
    stats: Option<RequestStats>,
    speed: Option<OutputSpeed>,
) -> App {
    let (background, foreground) = if light {
        ((255, 255, 255), (0, 0, 0))
    } else {
        ((0, 0, 0), (230, 230, 230))
    };
    let mut app = App::new(
        Palette::new(ColorLevel::TrueColor, Some(background)).with_foreground(Some(foreground)),
        lang,
        Transcript::new(
            selection(),
            PermissionMode::Default,
            crate::view::test_workspace(),
        ),
        Some("/Users/dev".into()),
        "0.1.0".into(),
    );
    // 66,600 / 180,000：ctx 37.0%。
    app.view.apply(
        SequencedEvent {
            seq: 1,
            revision: 0,
            event: KernelEvent::ContextUsage {
                used_tokens: 66_600,
                max_tokens: 180_000,
            },
        },
        0,
    );
    if let Some(speed) = speed {
        app.view.apply(
            SequencedEvent {
                seq: 2,
                revision: 0,
                event: KernelEvent::OutputSpeed { speed },
            },
            0,
        );
    }
    if let Some(stats) = stats {
        let seq = app.request_stats.begin();
        assert!(app.request_stats.arrived(seq, Some(stats)));
    }
    app
}

/// Node 调试面板的一轮（kernel-api `metering_tests` 同一组数）：累计命中率 0.91。F105 起回执里的 `last` 不再给 TPS。
fn node_stats() -> RequestStats {
    RequestStats {
        requests: 2,
        input_tokens: 2_000,
        cache_read_tokens: 1_820,
        unknown_cache_usage: false,
        last: None,
    }
}

/// 同一轮的输出速度：120 个输出 token、总时长 5000 ms、首个内容 2000 ms → 40 tok/s。
fn node_speed() -> OutputSpeed {
    OutputSpeed {
        output_tokens: Some(120),
        duration_ms: 5_000,
        first_content_ms: Some(2_000),
    }
}

fn plain(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

/// footer 右侧：依次是 TPS、ctx、cache，模型名贴最右（工作线 F116，tui-display.md §2.6）；文字用次要文字的颜色，
/// 中文用中文短词。
#[test]
fn the_request_stats_come_before_the_model_name() {
    for light in [false, true] {
        let app = stats_app(Lang::En, light, Some(node_stats()));
        let (line, _) = footer(&app, 160);
        let text = plain(&line);
        assert!(
            text.ends_with("40 tok/s \u{b7} ctx 37.0% \u{b7} cache 91.0% \u{b7} scripted"),
            "{text}"
        );
        for item in ["40 tok/s", "ctx 37.0%", "cache 91.0%"] {
            let span = line.spans.iter().find(|s| s.content == item).unwrap();
            assert_eq!(span.style, app.palette.secondary(), "{item} light={light}");
            assert!(span.style.fg.is_some(), "{item}: 真彩色的次要文字");
        }
        let zh = stats_app(Lang::Zh, light, Some(node_stats()));
        let text = plain(&footer(&zh, 160).0);
        assert!(
            text.ends_with("40 tok/s \u{b7} 上下文 37.0% \u{b7} 缓存 91.0% \u{b7} scripted"),
            "{text}"
        );
    }
}

/// ctx 与 cache 保留一位小数、四舍五入到 0.1，整数也写 `.0`；TPS 仍是整数（工作线 F115，用户决定 2026-10-29，
/// tui-display.md §2.6）。
#[test]
fn ctx_and_cache_show_one_decimal() {
    let ctx = |used_tokens, max_tokens, lang| {
        let stats = RequestStats {
            requests: 3,
            input_tokens: 3_000,
            cache_read_tokens: 2_000,
            unknown_cache_usage: false,
            last: None,
        };
        let mut app = stats_app(lang, false, Some(stats));
        app.view.apply(
            SequencedEvent {
                seq: 10,
                revision: 0,
                event: KernelEvent::ContextUsage {
                    used_tokens,
                    max_tokens,
                },
            },
            0,
        );
        plain(&footer(&app, 160).0)
    };
    // 1,120 / 180,000 = 0.62%：整数时写 1%；2,000 / 3,000 的命中率 66.67%。
    let text = ctx(1_120, 180_000, Lang::En);
    assert!(
        text.ends_with("40 tok/s \u{b7} ctx 0.6% \u{b7} cache 66.7% \u{b7} scripted"),
        "{text}"
    );
    let text = ctx(1_120, 180_000, Lang::Zh);
    assert!(
        text.ends_with("上下文 0.6% \u{b7} 缓存 66.7% \u{b7} scripted"),
        "{text}"
    );
    // 第二位小数四舍五入：9,108 / 180,000 = 5.06%，9,072 / 180,000 = 5.04%。
    assert!(ctx(9_108, 180_000, Lang::En).contains("ctx 5.1%"));
    assert!(ctx(9_072, 180_000, Lang::En).contains("ctx 5.0%"));
    // 用满与超出窗口都是 100.0%；窗口为 0 时也是 100.0%。
    assert!(ctx(180_000, 180_000, Lang::En).contains("ctx 100.0%"));
    assert!(ctx(200_000, 180_000, Lang::En).contains("ctx 100.0%"));
    assert!(ctx(5, 0, Lang::En).contains("ctx 100.0%"));
    assert!(ctx(0, 180_000, Lang::En).contains("ctx 0.0%"));
}

/// 没有值的项连同它前面的分隔点一起不显示：没有回执、TPS 没有值、命中率未知、内核没给上下文用量。
#[test]
fn items_without_values_are_left_out() {
    let none = stats_app(Lang::En, false, None);
    let text = plain(&footer(&none, 160).0);
    assert!(text.ends_with("ctx 37.0% \u{b7} scripted"), "{text}");
    let no_content = OutputSpeed {
        first_content_ms: None,
        ..node_speed()
    };
    let no_tps = speed_app(Lang::En, false, Some(node_stats()), Some(no_content));
    let text = plain(&footer(&no_tps, 160).0);
    assert!(
        text.ends_with("ctx 37.0% \u{b7} cache 91.0% \u{b7} scripted"),
        "{text}"
    );
    let unknown = stats_app(
        Lang::En,
        false,
        Some(RequestStats {
            unknown_cache_usage: true,
            ..node_stats()
        }),
    );
    let text = plain(&footer(&unknown, 160).0);
    assert!(
        text.ends_with("40 tok/s \u{b7} ctx 37.0% \u{b7} scripted"),
        "{text}"
    );
    // 新会话的草稿：没有上下文用量、也没有回执，只有模型名。
    let mut draft = stats_app(Lang::En, false, None);
    draft.reset();
    let text = plain(&footer(&draft, 160).0);
    assert!(text.ends_with("scripted"), "{text}");
    assert!(!text.contains("ctx") && !text.contains("tok/s"), "{text}");
}

/// 修复线 F105（tui-display.md §2.6、D26）：TPS 只取会话 actor 给的输出速度，不取用量表回执里的最近一行；没有速度
/// （冷打开、切换会话之后）时不显示，即使回执里有最近一行。旧代码按回执的 `last` 显示 40 tok/s。
#[test]
fn the_speed_comes_from_the_session_not_the_usage_rows() {
    let stored = RequestStats {
        last: Some(rust_cli_kernel_api::metering::LastRequest {
            output_tokens: Some(120),
            duration_ms: Some(5_000),
            first_token_ms: Some(2_000),
        }),
        ..node_stats()
    };
    let cold = speed_app(Lang::En, false, Some(stored), None);
    let text = plain(&footer(&cold, 160).0);
    assert!(
        text.ends_with("ctx 37.0% \u{b7} cache 91.0% \u{b7} scripted"),
        "{text}"
    );
    let live = speed_app(Lang::En, false, Some(node_stats()), Some(node_speed()));
    let text = plain(&footer(&live, 160).0);
    // TPS 是右侧第一项（F116），后面接 ctx。
    assert!(text.contains(" 40 tok/s \u{b7} ctx 37.0%"), "{text}");
}

/// 切换会话与 `/clear` 之后，晚到的旧回执不覆盖（回执带序号，tui-display.md §2.6）。
#[test]
fn a_late_receipt_from_the_old_session_is_dropped() {
    let mut app = stats_app(Lang::En, false, None);
    let old = app.request_stats.begin();
    app.reset();
    assert!(!app.request_stats.arrived(old, Some(node_stats())));
    assert_eq!(app.request_stats.stats(), &RequestStats::default());
    let new = app.request_stats.begin();
    assert!(app.request_stats.arrived(new, Some(node_stats())));
    // 查询失败（存储不支持）时当作没有值。
    let again = app.request_stats.begin();
    assert!(app.request_stats.arrived(again, None));
    assert_eq!(app.request_stats.stats(), &RequestStats::default());
}

/// 窄终端：依次省掉 `? for shortcuts`、cache、TPS、ctx、上下文提示，最后才省模型名；模型名不被统计挤掉。
#[test]
fn narrow_terminals_drop_cache_then_tps_then_context() {
    let mut app = stats_app(Lang::En, false, Some(node_stats()));
    app.view.apply(
        SequencedEvent {
            seq: 3,
            revision: 0,
            event: KernelEvent::ContextNotice {
                notice: ContextNotice::UntilAutoCompact { percent: 10 },
            },
        },
        0,
    );
    let mut seen: Vec<(bool, String)> = Vec::new();
    for width in (20..=160).rev() {
        let text = plain(&footer(&app, width).0);
        let shortcuts = text.contains("? for shortcuts");
        // 按显示的先后列：统计、提示，模型名贴最右（F116）。
        let right: Vec<&str> = [
            "40 tok/s",
            "ctx 37.0%",
            "cache 91.0%",
            "10% until auto-compact",
            "scripted",
        ]
        .into_iter()
        .filter(|item| text.contains(item))
        .collect();
        let right = right.join(" | ");
        if seen.last().map(|(s, r)| (*s, r.as_str())) != Some((shortcuts, right.as_str())) {
            seen.push((shortcuts, right));
        }
    }
    let steps: Vec<(bool, &str)> = seen.iter().map(|(s, r)| (*s, r.as_str())).collect();
    assert_eq!(
        steps,
        [
            (
                true,
                "40 tok/s | ctx 37.0% | cache 91.0% | 10% until auto-compact | scripted"
            ),
            (
                false,
                "40 tok/s | ctx 37.0% | cache 91.0% | 10% until auto-compact | scripted"
            ),
            (
                false,
                "40 tok/s | ctx 37.0% | 10% until auto-compact | scripted"
            ),
            (false, "ctx 37.0% | 10% until auto-compact | scripted"),
            (false, "10% until auto-compact | scripted"),
            (false, "scripted"),
            (false, ""),
        ]
    );
}

/// 修复线 F117（tui-display.md §2.3）：pill 写 `1 shell, 2 monitors`；终端变窄时先省右侧，pill 的文字不拆开、不缩写，
/// 放不下时整行从末尾截断。
#[test]
fn a_narrow_footer_keeps_the_monitor_count_whole_until_the_line_is_cut() {
    use rust_cli_kernel_api::env_shell::BackgroundStatus;
    use rust_cli_kernel_api::manage::{BackgroundEntry, BackgroundKind};
    let mut app = app(Lang::En, ContextNotice::Hidden);
    let entry = |id: &str, kind| BackgroundEntry {
        id: id.into(),
        kind,
        description: id.into(),
        status: BackgroundStatus::Running,
        output: None,
        machine: None,
        reconnecting: false,
    };
    app.background = vec![
        entry("b1", BackgroundKind::Shell),
        entry("b2", BackgroundKind::Monitor),
        entry("s3", BackgroundKind::Monitor),
    ];
    let pill = "1 shell, 2 monitors";
    let wide = plain(&footer(&app, 160).0);
    assert!(wide.contains(&format!(" · {pill} · ↓ to manage")), "{wide}");
    assert!(wide.ends_with("scripted"), "{wide}");
    let tip = "↓ to manage";
    let left = &wide[..wide.find(tip).unwrap() + tip.len()];
    let mut cut = false;
    for width in (12..=160).rev() {
        let text = plain(&footer(&app, width).0);
        assert!(text.width() <= width, "{width}: {text}");
        if text.contains(pill) {
            assert!(!cut, "the pill came back at {width}: {text}");
            continue;
        }
        // 整行截断之前右侧已经省光；截断时只从末尾切掉一段，前面（含 pill 的前半）原样。
        cut = true;
        assert!(!text.contains("scripted"), "{width}: {text}");
        assert!(
            left.starts_with(text.trim_end_matches('…')),
            "{width}: {text}"
        );
    }
    assert!(cut);
}
