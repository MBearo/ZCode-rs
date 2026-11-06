use rust_cli_kernel_api::compaction::ContextNotice;
use rust_cli_kernel_api::metering::{OutputSpeed, RequestStats};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{KernelEvent, SequencedEvent};
use rust_cli_kernel_api::testing::selection;

use super::*;
use crate::ui::probe::{ColorLevel, Colors};
use crate::view::Transcript;

fn app(lang: Lang, notice: ContextNotice) -> App {
    let mut app = crate::ui::test_support::app_in(lang);
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

/// 状态栏（宽 `width`、终端 30 行）各行的文字。
fn bar(app: &App, width: usize) -> Vec<String> {
    status(app, width, 30, Vec::new(), false)
        .iter()
        .map(StyledLine::plain)
        .collect()
}

/// 状态栏里上下文提示那一段的文字与样式：这些 App 没有用量与速度，提示是右边唯一的部分，是最后一段非空白的段。
fn context(app: &App) -> (String, Style) {
    let lines = status(app, 200, 30, Vec::new(), false);
    let span = lines[0]
        .spans
        .iter()
        .rev()
        .find(|s| !s.text.trim().is_empty())
        .unwrap();
    (span.text.to_string(), span.style)
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
        let ink = &app(Lang::En, notice).ink;
        if matches!(notice, ContextNotice::Low { .. }) {
            // 自动压缩关闭时是快满的琥珀色（`code`，Puffin 的 `near`，tui-kit.md §3.4）。
            assert_eq!(style, Style::fg(ink.theme.code), "{text}");
        } else {
            assert_eq!(style, ink.theme.fg2, "{text}");
        }
    }
    let plain = bar(&app(Lang::En, ContextNotice::Hidden), 160).join("\n");
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
        Colors::new(ColorLevel::TrueColor, Some(background)).with_foreground(Some(foreground)),
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
                compact_at: None,
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
        let seq = app.snapshots.request_stats.begin(0);
        assert!(app.snapshots.request_stats.arrived(seq, Some(stats)));
    }
    // 画一次：状态栏的速度是画之前听说视图的现场估算（status_motion.rs），没有在跑的调用时说量到的数。
    let started = app.started;
    crate::ui::status_motion::note(&mut app, started, 0);
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
        ..RequestStats::default()
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

/// 状态栏照 Puffin（tui-kit.md §4.2）：左边是模式与 `⇧⇥`、模型、cwd；右边是上下文的表、数与量、速度；缓存命中率不在这里，
/// 只在 `/stats` 的卡里（tui-display.md §2.6）。数 fg2、单位与窗口的大小 fg3。
#[test]
fn the_status_bar_says_who_where_and_how_much() {
    for light in [false, true] {
        let app = stats_app(Lang::En, light, Some(node_stats()));
        let shown = bar(&app, 160);
        assert_eq!(shown.len(), 1, "{shown:?}");
        let line = &shown[0];
        assert!(
            line.starts_with("   Ask before changes ⇧⇥  ·  scripted  ·  ~/project"),
            "{line}"
        );
        assert!(
            line.ends_with("━━━━────── 37%  67k/180k  ·  40 tok/s "),
            "{line}"
        );
        let lines = status(&app, 160, 30, Vec::new(), false);
        let span = |text: &str| {
            lines[0]
                .spans
                .iter()
                .find(|s| s.text == text)
                .unwrap_or_else(|| panic!("{text}"))
                .style
        };
        assert_eq!(span("37%"), app.ink.theme.fg2);
        assert_eq!(span("/180k"), app.ink.theme.fg3);
        assert_eq!(span(" tok/s"), app.ink.theme.fg3);
        assert_eq!(span("⇧⇥"), app.ink.theme.line);
        assert_eq!(span("Ask before changes"), app.ink.theme.fg3);
    }
    let zh = stats_app(Lang::Zh, false, Some(node_stats()));
    let line = bar(&zh, 160).join("");
    assert!(line.starts_with("   变更前确认 ⇧⇥"), "{line}");
    assert!(!line.contains("缓存"), "{line}");
}

/// 没有值的部分不出现：没有速度时没有 tok/s，没有用量时没有表。
#[test]
fn parts_without_values_are_left_out() {
    let none = speed_app(Lang::En, false, None, None);
    let text = bar(&none, 160).join("");
    assert!(text.trim_end().ends_with("37%  67k/180k"), "{text}");
    assert!(!text.contains("tok/s"), "{text}");
    let unknown = speed_app(
        Lang::En,
        false,
        Some(RequestStats {
            unknown_cache_usage: true,
            ..node_stats()
        }),
        Some(node_speed()),
    );
    let text = bar(&unknown, 160).join("");
    assert!(
        text.contains("40 tok/s") && !text.contains("cache"),
        "{text}"
    );
    let draft = app(Lang::En, ContextNotice::Hidden);
    let text = bar(&draft, 160).join("");
    assert!(text.trim_end().ends_with("~/project"), "{text}");
}

/// F105：速度取会话 actor 给的输出速度，不取用量表。
#[test]
fn the_speed_comes_from_the_session_not_the_usage_rows() {
    let cold = speed_app(Lang::En, false, Some(node_stats()), None);
    assert!(!bar(&cold, 160).join("").contains("tok/s"));
    let live = speed_app(Lang::En, false, None, Some(node_speed()));
    assert!(bar(&live, 160).join("").contains("  ·  40 tok/s"));
}

#[test]
fn a_late_receipt_from_the_old_session_is_dropped() {
    let mut app = stats_app(Lang::En, false, None);
    let old = app.snapshots.request_stats.begin(0);
    app.reset();
    assert!(!app.snapshots.request_stats.arrived(old, Some(node_stats())));
    assert_eq!(
        app.snapshots.request_stats.stats(),
        &RequestStats::default()
    );
    let new = app.snapshots.request_stats.begin(0);
    assert!(app.snapshots.request_stats.arrived(new, Some(node_stats())));
    // 查询失败（存储不支持）时当作没有值。
    let again = app.snapshots.request_stats.begin(0);
    assert!(app.snapshots.request_stats.arrived(again, None));
    assert_eq!(
        app.snapshots.request_stats.stats(),
        &RequestStats::default()
    );
}

/// 窄终端照 Puffin 的名次丢（docs/puffin-tui.md §4.2「丢弃的次序」）：名次低的先走（缓存、窗口的大小、速度、用掉的数、
/// cwd……），模式与百分数最后走；终端至少 20 行时，一行要丢掉不是附加的部分而两行留得住它时分成两行。
#[test]
fn narrow_terminals_drop_parts_by_rank_or_split_in_two() {
    let app = stats_app(Lang::En, false, Some(node_stats()));
    let at = |width: usize, height: usize| -> Vec<String> {
        status(&app, width, height, Vec::new(), false)
            .iter()
            .map(StyledLine::plain)
            .collect()
    };
    // 名次低的先走，再按名次从高到低放回放得下的（Puffin `fit`）：`⇧⇥` 很短，大的部分走了以后它回来。
    let one = at(70, 10);
    assert_eq!(one.len(), 1, "{one:?}");
    assert!(
        !one[0].contains("67k") && !one[0].contains("cache"),
        "{one:?}"
    );
    assert!(
        one[0].contains("Ask before changes") && one[0].contains("37%"),
        "{one:?}"
    );
    let two = at(70, 30);
    assert_eq!(two.len(), 2, "{two:?}");
    assert!(
        two[0].contains("scripted") && two[0].contains("~/project"),
        "{two:?}"
    );
    assert!(two[1].starts_with("   ━━━━──────"), "{two:?}");
    // 模式（名次 100）最后走，百分数（90）其次。
    let small = at(30, 10);
    assert_eq!(
        small[0].trim_end(),
        "   Ask before changes     37%",
        "{small:?}"
    );
    let tiny = at(24, 10);
    assert_eq!(tiny[0].trim_end(), "   Ask before changes", "{tiny:?}");
}

/// 槽（docs/puffin-tui.md §4.2「按键提示（槽）」）：空的输入框上是 Puffin 的 `? keys`，在输入行的右端，止于 `edge`；占位是
/// `Send a message`。键 fg2、说明 fg3。以前是 `Try "…"` 与 `← for projects · ? for shortcuts`。
#[test]
fn the_slot_sits_at_the_right_of_the_input_row() {
    let app = stats_app(Lang::En, false, None);
    let line = crate::ui::pane::input_line(&app, 100);
    let text = line.plain();
    assert!(text.starts_with(" ›  Send a message"), "{text}");
    assert!(text.ends_with("? keys"), "{text}");
    assert_eq!(text.chars().count(), 99, "{text}");
    let key = line.spans.iter().find(|s| s.text == "?").unwrap();
    assert_eq!(key.style, app.ink.key());
    let zh = stats_app(Lang::Zh, false, None);
    let text = crate::ui::pane::input_line(&zh, 100).plain();
    assert!(text.starts_with(" ›  发送一条消息"), "{text}");
    assert!(text.ends_with("? 按键"), "{text}");
}

/// 别的代理有消息时（tui-cockpit.md §3.4）的 App：现在的会话 `current`，`kinds` 是 runner 读来的组。
fn news_app(kinds: &[(&str, crate::ui::cockpit::Kind)]) -> App {
    let mut app = stats_app(Lang::En, false, None);
    app.session = Some("current".to_owned());
    app.cockpit_news = kinds.iter().map(|(id, k)| ((*id).to_owned(), *k)).collect();
    app
}

fn slot_text(app: &App) -> String {
    slot(app).spans.iter().map(|s| s.text.as_ref()).collect()
}

/// 槽的 ← 提示（tui-cockpit.md §3.4）：一直写着 `← agent cockpit`，空闲时在 `? keys` 前；别的代理有消息时说明换成数，
/// 按急的次序只数一种，不数现在的会话；记号是组的墨，数 fg3；回合在跑时在 `esc stop` 后；栈里有东西时在
/// `⌫ take back` 后；有草稿时没有（← 移动光标）。
#[test]
fn the_slot_always_points_at_the_cockpit_and_counts_news() {
    use crate::ui::cockpit::Kind;
    // 没有消息：名字。Working 不算消息；现在的会话不数。
    let quiet = news_app(&[("a", Kind::Work), ("current", Kind::Need)]);
    assert_eq!(slot_text(&quiet), "← agent cockpit  ? keys");
    let spans = slot(&quiet).spans;
    assert_eq!(spans[0].text, "←");
    assert_eq!(spans[0].style, quiet.ink.theme.fg2);
    assert_eq!(spans[1].style, quiet.ink.theme.fg3);
    let app = news_app(&[("a", Kind::New), ("b", Kind::Need), ("c", Kind::Need)]);
    assert_eq!(slot_text(&app), "← ● 2 need you  ? keys");
    let spans = slot(&app).spans;
    let mark = spans.iter().find(|s| s.text == "●").unwrap();
    assert_eq!(mark.style, Style::fg(app.ink.theme.code));
    let words = spans.iter().find(|s| s.text == " 2 need you").unwrap();
    assert_eq!(words.style, app.ink.theme.fg3);
    // 失败的比新的急。
    let failed = news_app(&[("a", Kind::New), ("b", Kind::Failed)]);
    assert_eq!(slot_text(&failed), "← ✗ 1 failed  ? keys");
    assert_eq!(
        slot_text(&news_app(&[("a", Kind::New)])),
        "← ○ 1 new  ? keys"
    );
    // 中文同页头一行的数。
    let mut zh = news_app(&[("b", Kind::Need)]);
    zh.lang = Lang::Zh;
    assert_eq!(slot_text(&zh), "← ● 1 个等你  ? 按键");
    zh.cockpit_news.clear();
    assert_eq!(slot_text(&zh), "← 代理驾驶舱  ? 按键");
    // 有草稿：← 移动光标，没有提示。
    let mut typing = news_app(&[("b", Kind::Need)]);
    typing.composer.set_text("hi");
    assert!(!slot_text(&typing).contains('←'), "{}", slot_text(&typing));
    // 栈里有东西。
    let mut stacked = news_app(&[("b", Kind::Need)]);
    stacked
        .after_turn
        .push(("/clear".to_owned(), crate::ui::app::Action::Interrupt));
    assert_eq!(slot_text(&stacked), "⌫ take back  ← ● 1 needs you");
    // 回合在跑。
    let mut running = news_app(&[("b", Kind::Need)]);
    running.view.apply(
        SequencedEvent {
            seq: 9,
            revision: 0,
            event: KernelEvent::TurnStarted {
                turn: rust_cli_kernel_api::ids::TurnId::new("turn_1"),
                command: None,
            },
        },
        0,
    );
    assert_eq!(slot_text(&running), "esc stop  ← ● 1 needs you");
    running.cockpit_news.clear();
    assert_eq!(slot_text(&running), "esc stop  ← agent cockpit");
}

/// 点槽的提示同按 ←：打开驾驶舱（tui-cockpit.md §3.4）；`? keys` 照旧打开按键表。名字与数都能点。
#[test]
fn clicking_the_cockpit_hint_opens_the_cockpit() {
    use crate::ui::cockpit::Kind;
    let quiet = news_app(&[]);
    let line = crate::ui::pane::input_line(&quiet, 100);
    let targets = slot_targets(&quiet, &line);
    assert!(
        targets
            .iter()
            .any(|(_, w, t)| matches!(t, Target::OpenCockpit)
                && *w == string_width("← agent cockpit")),
        "{}",
        line.plain()
    );
    let app = news_app(&[("b", Kind::Need)]);
    let line = crate::ui::pane::input_line(&app, 100);
    let text = line.plain();
    let targets = slot_targets(&app, &line);
    let (at, width, _) = targets
        .iter()
        .find(|(_, _, t)| matches!(t, Target::OpenCockpit))
        .unwrap_or_else(|| panic!("{text}"));
    assert_eq!(*width, string_width("← ● 1 needs you"));
    assert!(
        text[text.char_indices().nth(*at).unwrap().0..].starts_with("← ● 1 needs you"),
        "{text}"
    );
    assert!(
        targets
            .iter()
            .any(|(_, _, t)| matches!(t, Target::Shortcuts))
    );
}
