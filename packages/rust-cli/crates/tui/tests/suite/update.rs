// 会话里“有新版本”的提示（docs/specs/rust-cli/update.md §7，工作线 F48）：照 Puffin 写在欢迎块的版本行尾
// （`welcome::block` 的 `X is out · zcode update`，tui-kit.md §4.2「欢迎块」）。恢复的会话的头也是欢迎块（tui.md §7.3），
// 同样有这句。启动前的选择屏是 `screens::ask_update`（tui-kit.md §2.3），它的画面与按键在 `src/screens/screens_tests.rs`。
use rust_cli_kernel_api::testing::MemoryStore;
use rust_cli_tui::update::UpdateInfo;

use super::sessions_seed::{Seed, seed};
use super::support::{GatedModel, Setup, with_ui};

fn info() -> UpdateInfo {
    UpdateInfo {
        current: "0.1.46".to_owned(),
        latest: "0.1.47".to_owned(),
        command: "npm install -g @scope/zcode".to_owned(),
        notes_url: "https://www.npmjs.com/package/@scope/zcode".to_owned(),
    }
}

fn line_of(screen: &str, needle: &str) -> usize {
    screen
        .lines()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("{needle} not on screen:\n{screen}"))
}

fn update_setup(lang: &'static str, store: Option<MemoryStore>) -> Setup {
    let mut setup = Setup {
        width: 80,
        height: 30,
        lang,
        ..Setup::default()
    };
    setup.ui.update = Some(info());
    if let Some(store) = store {
        setup.store = std::sync::Arc::new(store);
    }
    setup
}

/// 会话里的提示：欢迎块的版本行尾 fg2 的 `0.1.47 is out · ` 与 brand 的 `zcode update`。以前是会话头下面的三行。
#[tokio::test(flavor = "multi_thread")]
async fn the_notice_sits_on_the_version_row() {
    let model = GatedModel::scripted(Vec::new());
    with_ui(
        model,
        update_setup("en_US.UTF-8", None),
        |mut ui| async move {
            let screen = ui.wait_for("0.1.47 is out · zcode update").await;
            let row = line_of(&screen, "0.1.47 is out");
            assert_eq!(row, line_of(&screen, "ZCode  "), "{screen}");
            assert!(!screen.contains("Update available!"), "{screen}");
        },
    )
    .await;
}

/// 中文环境下也在版本行尾，同一个位置与墨色，话是译文（`i18n_kit::is_out`）；命令照旧是 `zcode update`。
#[tokio::test(flavor = "multi_thread")]
async fn the_notice_shows_in_a_chinese_locale() {
    let model = GatedModel::scripted(Vec::new());
    with_ui(
        model,
        update_setup("zh_CN.UTF-8", None),
        |mut ui| async move {
            let screen = ui.wait_for("0.1.47 已发布 · zcode update").await;
            assert!(!screen.contains("有新版本！"), "{screen}");
        },
    )
    .await;
}

/// 启动时打开已有会话（`--resume`）：头是欢迎块，文件夹下面紧接标题（tui.md §7.3），也有这句。
#[tokio::test(flavor = "multi_thread")]
async fn a_resumed_session_heads_with_its_title() {
    let store = MemoryStore::default();
    seed(
        &store,
        &[Seed {
            id: "sess_resumed",
            root: "/Users/dev/project",
            title: "Old work",
            prompt: "earlier question",
            reply: "earlier answer",
            created: 1,
            updated: 2,
        }],
    )
    .await;
    let mut setup = update_setup("en_US.UTF-8", Some(store));
    setup.startup = rust_cli_tui::Startup::Session("sess_resumed".to_owned());
    let model = GatedModel::scripted(Vec::new());
    with_ui(model, setup, |mut ui| async move {
        let screen = ui.wait_for("earlier answer").await;
        let head = line_of(&screen, "Old work  started");
        assert!(head < line_of(&screen, "earlier question"), "{screen}");
        assert_eq!(head, line_of(&screen, "ZCode  ") + 2, "{screen}");
        assert!(screen.contains("is out"), "{screen}");
    })
    .await;
}
