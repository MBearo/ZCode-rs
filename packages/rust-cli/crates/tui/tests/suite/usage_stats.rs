//! `/usage` 的 Stats 标签页（docs/specs/rust-cli/usage.md §5）：内核的活动统计、Overview 与 Models、`r` 换日期
//! 范围、Usage 标签页，深浅主题与中英文。
use crossterm::event::KeyCode;
use ratatui::style::Color;
use rust_cli_kernel_api::metering::{ActivityDay, ActivityRange, ActivityStats, ModelTokens};
use rust_cli_tui::ui::palette::{ColorLevel, Palette};

use super::support::{GatedModel, Setup, with_ui};

/// 测试的时钟是 0：今天是 1970-01-01（周四）。
fn all_time() -> ActivityStats {
    ActivityStats {
        range: Some(ActivityRange::All),
        today: "1970-01-01".into(),
        days: vec![ActivityDay {
            date: "1970-01-01".into(),
            messages: 5,
        }],
        models: vec![
            ModelTokens {
                model: "opus".into(),
                input: 900,
                output: 100,
                cache_read: 0,
                cache_write: 0,
            },
            ModelTokens {
                model: "haiku".into(),
                input: 40,
                output: 10,
                cache_read: 0,
                cache_write: 0,
            },
        ],
        sessions: 3,
        longest_session_ms: Some(65_000),
        first_session_ms: Some(0),
        last_session_ms: Some(0),
    }
}

fn setup(lang: &'static str, background: (u8, u8, u8)) -> Setup {
    let setup = Setup {
        height: 40,
        lang,
        palette: Palette::new(ColorLevel::TrueColor, Some(background)),
        ..Setup::default()
    };
    setup.store.set_activity(ActivityRange::All, all_time());
    setup.store.set_activity(
        ActivityRange::Week,
        ActivityStats {
            range: Some(ActivityRange::Week),
            sessions: 7,
            ..all_time()
        },
    );
    setup
}

#[tokio::test(flavor = "multi_thread")]
async fn stats_shows_the_overview_cycles_dates_and_lists_models() {
    let model = GatedModel::scripted(Vec::new());
    with_ui(
        model.clone(),
        setup("en_US.UTF-8", (0, 0, 0)),
        |mut ui| async move {
            ui.wait_for("Ask before changes").await;
            ui.submit("/stats");
            let page = ui.wait_for("Favorite model: opus").await;
            assert!(page.contains("Overview"), "{page}");
            assert!(page.contains("Total tokens: 1.1k"), "{page}");
            assert!(page.contains("Sessions: 3"), "{page}");
            assert!(page.contains("Longest session: 1m 5s"), "{page}");
            assert!(page.contains("Active days: 1/1"), "{page}");
            assert!(page.contains("Current streak: 1 day"), "{page}");
            assert!(page.contains("Most active day: Jan 1"), "{page}");
            assert!(
                page.contains("Less \u{2591} \u{2592} \u{2593} \u{2588} More"),
                "{page}"
            );
            // r：Last 7 days 由内核查（MemoryStore 的预设），分母是 7。
            ui.key(KeyCode::Char('r'));
            let week = ui.wait_for("Sessions: 7").await;
            assert!(week.contains("Active days: 1/7"), "{week}");
            // r：Last 30 days 没有预设，内核给出空的统计（不是空库：全时段仍有会话）。
            ui.key(KeyCode::Char('r'));
            ui.wait_for("Sessions: 0").await;
            ui.key(KeyCode::Char('r'));
            ui.wait_for("Sessions: 3").await;
            ui.key(KeyCode::Right);
            let models = ui.wait_for("\u{2022} opus (").await;
            assert!(models.contains("\u{2022} haiku ("), "{models}");
            assert!(models.contains("In: 900 \u{b7} Out: 100"), "{models}");
            ui.key(KeyCode::Tab);
            let usage = ui.wait_for("Total duration (API):").await;
            assert!(usage.contains("Session"), "{usage}");
            assert!(usage.contains("Esc to cancel"), "{usage}");
            ui.key(KeyCode::Esc);
            ui.wait("the page to close", |s| !s.contains("Total duration"))
                .await;
        },
    )
    .await;
    assert!(
        model.requests().is_empty(),
        "/stats never reaches the model"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn chinese_and_light_backgrounds_keep_the_page() {
    let model = GatedModel::scripted(Vec::new());
    let buffer = std::sync::Arc::new(std::sync::Mutex::new(None));
    let seen = buffer.clone();
    with_ui(
        model,
        setup("zh_CN.UTF-8", (255, 255, 255)),
        |mut ui| async move {
            ui.submit("/usage");
            // 会话部分的输出格式固定，中英文界面相同（usage.md D10）；标签页与提示按界面语言。
            let page = ui.wait_for("Total duration (API):").await;
            assert!(page.contains("会话"), "{page}");
            assert!(page.contains("Esc 取消"), "{page}");
            assert!(page.contains("tab 切换标签页"), "{page}");
            ui.key(KeyCode::Tab);
            let stats = ui.wait_for("最常用的模型： opus").await;
            assert!(stats.contains("概览"), "{stats}");
            assert!(stats.contains("当前连续： 1 天"), "{stats}");
            *seen.lock().unwrap() = ui.buffer.lock().unwrap().clone();
        },
    )
    .await;
    // 浅色背景：选中的标签页用浅色主题的强调色（深蓝），不是深色主题的浅蓝。
    let buffer = buffer.lock().unwrap().clone().unwrap();
    let area = buffer.area;
    let accent = (area.left()..area.right())
        .map(|x| buffer[(x, area.top())].clone())
        .find(|cell| cell.symbol() == "统")
        .map(|cell| cell.fg);
    assert_eq!(accent, Some(Color::Rgb(0x1c, 0x64, 0xc8)));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_empty_library_says_there_are_no_stats_yet() {
    let model = GatedModel::scripted(Vec::new());
    let setup = Setup {
        height: 40,
        ..Setup::default()
    };
    with_ui(model, setup, |mut ui| async move {
        ui.submit("/stats");
        ui.wait_for("No stats available yet. Start using ZCode!")
            .await;
    })
    .await;
}
