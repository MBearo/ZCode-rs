//! `/usage all` 打开的「This computer」页（docs/specs/rust-cli/usage.md §5；tui-kit.md §4.3）：会话的 API 时长、内核的
//! 本机活动统计（热力图、范围内的合计），←→ 换范围，esc 回到用量卡；深浅主题。以前是旧界面的 Usage 与 Stats 两个标签页。
use crossterm::event::KeyCode;
use rust_cli_base::models::{GLM_4_6, GLM_5_3};
use rust_cli_kernel_api::metering::{ActivityDay, ActivityRange, ActivityStats, ModelTokens};
use rust_cli_tui::{ColorLevel, Colors};

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
                model: GLM_5_3.into(),
                input: 900,
                output: 100,
                cache_read: 0,
                cache_write: 0,
            },
            ModelTokens {
                model: GLM_4_6.into(),
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

/// 报告了前景（反色）的终端：rich 的主题（tui-kit.md §3.4）。
fn setup(lang: &'static str, background: (u8, u8, u8)) -> Setup {
    let foreground = (255 - background.0, 255 - background.1, 255 - background.2);
    let setup = Setup {
        height: 40,
        lang,
        colors: Colors::new(ColorLevel::TrueColor, Some(background))
            .with_foreground(Some(foreground)),
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
async fn this_computer_shows_the_session_the_heat_map_and_cycles_ranges() {
    let model = GatedModel::scripted(Vec::new());
    with_ui(
        model.clone(),
        setup("en_US.UTF-8", (0, 0, 0)),
        |mut ui| async move {
            ui.wait_for("Ask before changes").await;
            ui.submit("/usage all");
            let page = ui.wait_for("Usage  ›  This computer").await;
            assert!(page.contains("This session"), "{page}");
            assert!(page.contains("API time"), "{page}");
            // Puffin 的热力图（kit `heat_rows`）：段标题、半块的格子。
            assert!(page.contains("Messages, last 52 weeks"), "{page}");
            assert!(page.contains("5 total"), "{page}");
            assert!(page.contains("\u{2580}"), "{page}");
            assert!(
                page.contains("On this computer   All time  7 days  30 days"),
                "{page}"
            );
            assert!(page.contains("3 sessions"), "{page}");
            assert!(page.contains(GLM_5_3), "{page}");
            assert!(page.contains("1m session"), "{page}");
            // 页的键在槽里（Puffin 的页）。
            assert!(page.contains("←→ range"), "{page}");
            // →：Last 7 days 由内核查（MemoryStore 的预设）。
            ui.key(KeyCode::Right);
            ui.wait_for("7 sessions").await;
            // →：Last 30 days 没有预设，内核给出空的统计。
            ui.key(KeyCode::Right);
            ui.wait("the empty range", |s| {
                s.contains("No activity yet.") && !s.contains("7 sessions")
            })
            .await;
            ui.key(KeyCode::Right);
            ui.wait_for("3 sessions").await;
            // esc 回到用量卡（测试没有用量的来源：卡说读不到），再 esc 关上。
            ui.key(KeyCode::Esc);
            ui.wait_for("Could not read your usage").await;
            ui.key(KeyCode::Esc);
            ui.wait("the card to close", |s| {
                !s.contains("Could not read your usage")
            })
            .await;
        },
    )
    .await;
    assert!(
        model.requests().is_empty(),
        "/usage all never reaches the model"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn light_backgrounds_choose_the_range_in_the_light_brand() {
    let model = GatedModel::scripted(Vec::new());
    let buffer = std::sync::Arc::new(std::sync::Mutex::new(None));
    let seen = buffer.clone();
    with_ui(
        model,
        setup("en_US.UTF-8", (255, 255, 255)),
        |mut ui| async move {
            ui.submit("/usage all");
            ui.wait_for("On this computer").await;
            *seen.lock().unwrap() = ui.shot.lock().unwrap().clone();
        },
    )
    .await;
    // 选中的范围是浅色主题的强调色（`Ink::accent`，`brand`），与深色主题的不同。
    let ink = |bg: (u8, u8, u8)| {
        rust_cli_tui::Ink::new(
            ColorLevel::TrueColor,
            Some((255 - bg.0, 255 - bg.1, 255 - bg.2)),
            Some(bg),
        )
    };
    let (light, dark) = (ink((255, 255, 255)).accent(), ink((0, 0, 0)).accent());
    assert_ne!(light, dark);
    let buffer = buffer.lock().unwrap().clone().unwrap();
    let area = buffer.area;
    let row = (area.top()..area.bottom())
        .find(|&y| {
            let text: String = (area.left()..area.right())
                .map(|x| buffer[(x, y)].symbol().to_owned())
                .collect();
            text.contains("On this computer")
        })
        .expect("the totals heading");
    let text: Vec<String> = (area.left()..area.right())
        .map(|x| buffer[(x, row)].symbol().to_owned())
        .collect();
    let at = text
        .windows(3)
        .position(|w| w.concat() == "All")
        .expect("All time");
    assert_eq!(buffer[(area.left() + at as u16, row)].fg, light);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_empty_library_says_there_is_no_activity_yet() {
    let model = GatedModel::scripted(Vec::new());
    let setup = Setup {
        height: 40,
        ..Setup::default()
    };
    with_ui(model, setup, |mut ui| async move {
        ui.submit("/usage all");
        ui.wait("both empty sections", |s| {
            s.contains("On this computer") && s.matches("No activity yet.").count() == 2
        })
        .await;
    })
    .await;
}
