use ratatui::style::{Color, Modifier};
use rust_cli_kernel_api::compaction::ContextNotice;
use rust_cli_kernel_api::metering::RequestStats;
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
            event: KernelEvent::ContextNotice { notice },
        },
        0,
    );
    app
}

/// 右侧最后一段：上下文提示的文字与样式。
fn context(app: &App) -> (String, Style) {
    let (line, _) = footer(app, 160);
    let last = line.spans.last().unwrap();
    (last.content.to_string(), last.style)
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

/// F27 的 App：深色或浅色背景（前景色已知，次要文字是真彩色）、内核的上下文用量与一份请求统计的回执。
fn stats_app(lang: Lang, light: bool, stats: Option<RequestStats>) -> App {
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
    // 66,600 / 180,000：ctx 37%。
    app.view.apply(
        SequencedEvent {
            seq: 1,
            event: KernelEvent::ContextUsage {
                used_tokens: 66_600,
                max_tokens: 180_000,
            },
        },
        0,
    );
    if let Some(stats) = stats {
        let seq = app.request_stats.begin();
        assert!(app.request_stats.arrived(seq, Some(stats)));
    }
    app
}

/// Node 调试面板的一轮（kernel-api `metering_tests` 同一组数）：40 tok/s，累计命中率 0.91。
fn node_stats() -> RequestStats {
    RequestStats {
        requests: 2,
        input_tokens: 2_000,
        cache_read_tokens: 1_820,
        unknown_cache_usage: false,
        last: Some(rust_cli_kernel_api::metering::LastRequest {
            output_tokens: Some(120),
            duration_ms: Some(5_000),
            first_token_ms: Some(2_000),
        }),
    }
}

fn plain(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

/// footer 右侧：模型名之后依次是 TPS、ctx、cache（tui-display.md §2.6）；文字用次要文字的颜色，中文用中文短词。
#[test]
fn the_request_stats_follow_the_model_name() {
    for light in [false, true] {
        let app = stats_app(Lang::En, light, Some(node_stats()));
        let (line, _) = footer(&app, 160);
        let text = plain(&line);
        assert!(
            text.ends_with("scripted \u{b7} 40 tok/s \u{b7} ctx 37% \u{b7} cache 91%"),
            "{text}"
        );
        for item in ["40 tok/s", "ctx 37%", "cache 91%"] {
            let span = line.spans.iter().find(|s| s.content == item).unwrap();
            assert_eq!(span.style, app.palette.secondary(), "{item} light={light}");
            assert!(span.style.fg.is_some(), "{item}: 真彩色的次要文字");
        }
        let zh = stats_app(Lang::Zh, light, Some(node_stats()));
        let text = plain(&footer(&zh, 160).0);
        assert!(
            text.ends_with("scripted \u{b7} 40 tok/s \u{b7} 上下文 37% \u{b7} 缓存 91%"),
            "{text}"
        );
    }
}

/// 没有值的项连同它前面的分隔点一起不显示：没有回执、TPS 没有值、命中率未知、内核没给上下文用量。
#[test]
fn items_without_values_are_left_out() {
    let none = stats_app(Lang::En, false, None);
    let text = plain(&footer(&none, 160).0);
    assert!(text.ends_with("scripted \u{b7} ctx 37%"), "{text}");
    let mut stats = node_stats();
    stats.last = Some(rust_cli_kernel_api::metering::LastRequest {
        first_token_ms: None,
        ..stats.last.unwrap()
    });
    let no_tps = stats_app(Lang::En, false, Some(stats));
    let text = plain(&footer(&no_tps, 160).0);
    assert!(
        text.ends_with("scripted \u{b7} ctx 37% \u{b7} cache 91%"),
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
        text.ends_with("scripted \u{b7} 40 tok/s \u{b7} ctx 37%"),
        "{text}"
    );
    // 新会话的草稿：没有上下文用量、也没有回执，只有模型名。
    let mut draft = stats_app(Lang::En, false, None);
    draft.reset();
    let text = plain(&footer(&draft, 160).0);
    assert!(text.ends_with("scripted"), "{text}");
    assert!(!text.contains("ctx") && !text.contains("tok/s"), "{text}");
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
            seq: 2,
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
        let right: Vec<&str> = [
            "scripted",
            "40 tok/s",
            "ctx 37%",
            "cache 91%",
            "10% until auto-compact",
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
                "scripted | 40 tok/s | ctx 37% | cache 91% | 10% until auto-compact"
            ),
            (
                false,
                "scripted | 40 tok/s | ctx 37% | cache 91% | 10% until auto-compact"
            ),
            (
                false,
                "scripted | 40 tok/s | ctx 37% | 10% until auto-compact"
            ),
            (false, "scripted | ctx 37% | 10% until auto-compact"),
            (false, "scripted | 10% until auto-compact"),
            (false, "scripted"),
            (false, ""),
        ]
    );
}
