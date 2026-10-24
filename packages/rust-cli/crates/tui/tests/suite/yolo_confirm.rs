//! 切到完全访问的二次确认（docs/specs/rust-cli/tui.md §7.8，工作线 F87，用户决定 2026-10-24：照 App 的确认框，
//! 是局部的小框）：shift+tab、`/mode yolo`、`/mode` 选择器三个入口都先弹框；没勾选时确认不可用，勾选后确认才发
//! 模式命令，取消留在原来的模式；框开着时不叠框；已经是完全访问、启动参数与恢复的会话都不弹；中英文是 App 的原文；
//! 颜色是 App 的 token（深浅两种背景、16 色终端）；窄终端折行。旧代码直接切换，这些测试都失败。
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::store::{
    CommitReceipt, Fact, FactBatch, LoadedSession, SessionFilter, SessionMeta, SessionStore,
    SessionSummary, StoreError,
};
use rust_cli_kernel_api::testing::{MemoryStore, text_response};
use rust_cli_tui::ui::palette::{ColorLevel, Palette};

use super::support::{GatedModel, Setup, Ui, row_of, with_ui};

const TITLE: &str = "⚠ Enable full access?";

fn idle(screen: &str, text: &str) -> bool {
    screen.contains(text) && !screen.contains("esc to interrupt")
}

fn shift_tab(ui: &Ui) {
    ui.press(KeyCode::BackTab, KeyModifiers::SHIFT);
}

/// 从变更前确认按两次 shift+tab：先到自主模式，再弹框。
async fn open_by_shift_tab(ui: &mut Ui, guarded: &str, title: &str) -> String {
    shift_tab(ui);
    ui.wait_for(guarded).await;
    shift_tab(ui);
    ui.wait_for(title).await
}

/// 去掉所有空白（折过行的中英文都能整段比较）。
fn joined(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// 会话提交的权限模式（`ExecutionChanged`），按顺序。
fn committed_modes(store: &MemoryStore, session: &str) -> Vec<PermissionMode> {
    store
        .facts(&SessionId::new(session))
        .into_iter()
        .filter_map(|fact| match fact {
            Fact::ExecutionChanged { state } => Some(state.mode),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn shift_tab_asks_before_entering_full_access() {
    let store = Arc::new(MemoryStore::default());
    let setup = Setup {
        store: store.clone(),
        ..Setup::default()
    };
    let model = GatedModel::scripted(vec![text_response(&["hello"])]);
    let session = with_ui(model, setup, |mut ui| async move {
        ui.submit("hi");
        ui.wait("the reply", |s| idle(s, "⏺ hello")).await;
        let screen = open_by_shift_tab(&mut ui, "⏵⏵ Autonomous mode", TITLE).await;
        // 局部的框：上面的对话照常显示，输入框与 footer 让位；焦点在取消，确认没勾选时 dim。
        assert!(screen.contains("⏺ hello"), "{screen}");
        assert!(!screen.contains("⏵⏵"), "{screen}");
        assert!(
            screen.contains("    [ ] I understand the risks"),
            "{screen}"
        );
        assert!(screen.contains("  ❯ 1. Cancel"), "{screen}");
        assert!(screen.contains("    2. Confirm"), "{screen}");
        // 80×24 放得下整个框：顶上的线没有被截掉。
        let title = row_of(&screen, TITLE).unwrap() as usize;
        assert!(
            screen.lines().nth(title - 1).unwrap().starts_with("────"),
            "{screen}"
        );
        // 没勾选时确认不可用：enter 不起作用，框还在（接着按 ↓ 回绕到勾选框）。
        ui.key(KeyCode::Down);
        ui.wait_for("  ❯ 2. Confirm").await;
        ui.key(KeyCode::Enter);
        ui.key(KeyCode::Down);
        ui.wait_for("  ❯ [ ] I understand the risks").await;
        // esc 取消：留在自主模式。
        ui.key(KeyCode::Esc);
        let screen = ui.wait("the dialog to close", |s| !s.contains(TITLE)).await;
        assert!(screen.contains("⏵⏵ Autonomous mode"), "{screen}");
        // 再打开：勾选只属于上一次，这次又没勾；勾上之后确认才切换。
        shift_tab(&ui);
        let screen = ui.wait_for(TITLE).await;
        assert!(
            screen.contains("    [ ] I understand the risks"),
            "{screen}"
        );
        ui.key(KeyCode::Up);
        ui.key(KeyCode::Char(' '));
        ui.wait_for("  ❯ [✔] I understand the risks").await;
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Down);
        ui.wait_for("  ❯ 2. Confirm").await;
        ui.key(KeyCode::Enter);
        let screen = ui
            .wait("full access", |s| {
                !s.contains(TITLE) && s.contains("⏵⏵ Full access")
            })
            .await;
        // shift+tab 不提示（与以前相同）。
        assert!(!screen.contains("Mode switched"), "{screen}");
    })
    .await
    .unwrap();
    // 建会话时的变更前确认之后，内核只收到过自主模式与确认之后的完全访问：没有先切再回滚。
    assert_eq!(
        committed_modes(&store, &session),
        [
            PermissionMode::Default,
            PermissionMode::Guarded,
            PermissionMode::BypassPermissions
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_mode_command_and_the_picker_ask_first() {
    let model = GatedModel::scripted(Vec::new());
    with_ui(model.clone(), Setup::default(), |mut ui| async move {
        ui.wait_for("⏸ Ask before changes").await;
        ui.submit("/mode yolo");
        ui.wait_for(TITLE).await;
        ui.key(KeyCode::Esc);
        let screen = ui.wait("the dialog to close", |s| !s.contains(TITLE)).await;
        assert!(screen.contains("⏸ Ask before changes"), "{screen}");
        assert!(!screen.contains("Mode switched"), "{screen}");
        // 选择器：数字键选完全访问也先问；ctrl+c 同取消。
        ui.submit("/mode");
        ui.wait_for("3. Full access").await;
        ui.key(KeyCode::Char('3'));
        ui.wait_for(TITLE).await;
        ui.press(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let screen = ui.wait("the dialog to close", |s| !s.contains(TITLE)).await;
        assert!(screen.contains("⏸ Ask before changes"), "{screen}");
        assert!(!screen.contains("Mode switched"), "{screen}");
        // 勾选框上 enter 同 space；确认之后才提示并切换。
        ui.submit("/mode YOLO");
        ui.wait_for(TITLE).await;
        ui.key(KeyCode::Up);
        ui.key(KeyCode::Enter);
        ui.wait_for("  ❯ [✔] I understand the risks").await;
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Enter);
        let screen = ui.wait_for("Mode switched to Full access.").await;
        assert!(screen.contains("⏵⏵ Full access"), "{screen}");
        // 已经是完全访问：两个入口都不再问。
        ui.submit("/mode yolo");
        let screen = ui
            .wait("the second switch", |s| {
                s.matches("Mode switched to Full access.").count() == 2
            })
            .await;
        assert!(!screen.contains(TITLE), "{screen}");
        ui.submit("/mode");
        ui.wait_for("3. Full access ✔").await;
        ui.key(KeyCode::Char('3'));
        let screen = ui
            .wait("the third switch", |s| {
                s.matches("Mode switched to Full access.").count() == 3
            })
            .await;
        assert!(!screen.contains(TITLE), "{screen}");
    })
    .await;
    assert!(model.requests().is_empty(), "no command reached the model");
}

/// 数字键（工作线 F92）：`1` 取消；没勾选时 `2` 不起作用、焦点不动；勾选后 `2` 直接确认（焦点在勾选框上也是）；别的数字
/// 不起作用。中英文界面相同。旧代码上数字键都不起作用，这项失败。
#[tokio::test(flavor = "multi_thread")]
async fn digits_choose_the_numbered_option_in_both_languages() {
    let cases = [
        (
            "en_US.UTF-8",
            ["⏵⏵ Autonomous mode", TITLE, "Cancel", "Confirm"],
            ["I understand the risks", "⏵⏵ Full access"],
        ),
        (
            "zh_CN.UTF-8",
            ["⏵⏵ 自主模式", "⚠ 要启用完全访问吗？", "取消", "确认启用"],
            ["我已了解风险", "⏵⏵ 完全访问"],
        ),
    ];
    for (lang, [guarded, title, cancel, confirm], [ack, full]) in cases {
        let setup = Setup {
            lang,
            ..Setup::default()
        };
        with_ui(
            GatedModel::scripted(Vec::new()),
            setup,
            |mut ui| async move {
                open_by_shift_tab(&mut ui, guarded, title).await;
                // 没勾选时 2 不起作用、焦点不动：接着按 ↓ 到确认（2 要是移了焦点，↓ 会回绕到勾选框）。
                ui.key(KeyCode::Char('2'));
                ui.key(KeyCode::Down);
                let screen = ui.wait_for(&format!("  ❯ 2. {confirm}")).await;
                assert!(screen.contains(&format!("    [ ] {ack}")), "{screen}");
                assert!(!screen.contains(full), "{screen}");
                // 焦点在确认上按 1：取消，留在自主模式。
                ui.key(KeyCode::Char('1'));
                let screen = ui.wait("the dialog to close", |s| !s.contains(title)).await;
                assert!(screen.contains(guarded), "{screen}");
                // 再打开：别的数字不起作用，框还在（接着勾选能画出来）。
                shift_tab(&ui);
                ui.wait_for(title).await;
                for digit in ['0', '3', '9'] {
                    ui.key(KeyCode::Char(digit));
                }
                ui.key(KeyCode::Up);
                ui.key(KeyCode::Char(' '));
                let screen = ui.wait_for(&format!("  ❯ [✔] {ack}")).await;
                assert_eq!(screen.matches(title).count(), 1, "{screen}");
                assert!(screen.contains(&format!("    1. {cancel}")), "{screen}");
                // 勾选后焦点还在勾选框上，按 2 直接确认启用。
                ui.key(KeyCode::Char('2'));
                ui.wait("full access", |s| !s.contains(title) && s.contains(full))
                    .await;
            },
        )
        .await;
    }
}

/// 框开着时再按 shift+tab：不叠第二个框，勾选与焦点不变。
#[tokio::test(flavor = "multi_thread")]
async fn the_dialog_does_not_stack() {
    let model = GatedModel::scripted(Vec::new());
    with_ui(model, Setup::default(), |mut ui| async move {
        ui.wait_for("⏸ Ask before changes").await;
        open_by_shift_tab(&mut ui, "⏵⏵ Autonomous mode", TITLE).await;
        ui.key(KeyCode::Up);
        ui.key(KeyCode::Char(' '));
        ui.wait_for("  ❯ [✔] I understand the risks").await;
        shift_tab(&ui);
        shift_tab(&ui);
        // 焦点仍在勾选框上：↓ 到取消（重新打开的框焦点在取消，↓ 会到确认）。
        ui.key(KeyCode::Down);
        let screen = ui.wait_for("  ❯ 1. Cancel").await;
        assert_eq!(screen.matches(TITLE).count(), 1, "{screen}");
        assert!(
            screen.contains("    [✔] I understand the risks"),
            "{screen}"
        );
        ui.key(KeyCode::Esc);
        let screen = ui.wait("the dialog to close", |s| !s.contains(TITLE)).await;
        assert!(screen.contains("⏵⏵ Autonomous mode"), "{screen}");
    })
    .await;
}

/// 冷加载时像 node-store 一样读回会话最后提交的权限模式（内存存储的替身不读回，`load` 的 `mode` 总是 None）。
struct ModeOnLoad(Arc<MemoryStore>);

#[async_trait::async_trait]
impl SessionStore for ModeOnLoad {
    async fn create(&self, meta: SessionMeta) -> Result<(), StoreError> {
        self.0.create(meta).await
    }

    async fn commit(
        &self,
        session: &SessionId,
        batch: FactBatch,
    ) -> Result<CommitReceipt, StoreError> {
        self.0.commit(session, batch).await
    }

    async fn load(&self, session: &SessionId) -> Result<LoadedSession, StoreError> {
        let mut loaded = self.0.load(session).await?;
        loaded.mode = committed_modes(&self.0, session.as_str()).last().copied();
        Ok(loaded)
    }

    async fn list(&self, filter: SessionFilter) -> Result<Vec<SessionSummary>, StoreError> {
        self.0.list(filter).await
    }
}

/// 启动参数是完全访问、恢复一个完全访问的会话（`--resume` 与 `/resume`）：直接是完全访问，不弹框。
#[tokio::test(flavor = "multi_thread")]
async fn starting_or_resuming_in_full_access_does_not_ask() {
    let store = Arc::new(MemoryStore::default());
    let setup = Setup {
        store: store.clone(),
        mode: PermissionMode::BypassPermissions,
        ..Setup::default()
    };
    let model = GatedModel::scripted(vec![text_response(&["hello"])]);
    let session = with_ui(model, setup, |mut ui| async move {
        let screen = ui.wait_for("⏵⏵ Full access").await;
        assert!(!screen.contains(TITLE), "{screen}");
        ui.submit("hi");
        let screen = ui.wait("the reply", |s| idle(s, "⏺ hello")).await;
        assert!(!screen.contains(TITLE), "{screen}");
    })
    .await
    .unwrap();
    // `--resume <id>`：启动参数是缺省模式，会话读回来是完全访问。
    let setup = Setup {
        session_store: Some(Arc::new(ModeOnLoad(store.clone()))),
        startup: rust_cli_tui::Startup::Session(session.clone()),
        ..Setup::default()
    };
    with_ui(
        GatedModel::scripted(Vec::new()),
        setup,
        |mut ui| async move {
            let screen = ui.wait("the resumed session", |s| idle(s, "⏺ hello")).await;
            assert!(screen.contains("⏵⏵ Full access"), "{screen}");
            assert!(!screen.contains(TITLE), "{screen}");
        },
    )
    .await;
    // 会话里 `/resume <id>`。
    let setup = Setup {
        session_store: Some(Arc::new(ModeOnLoad(store))),
        ..Setup::default()
    };
    with_ui(
        GatedModel::scripted(Vec::new()),
        setup,
        |mut ui| async move {
            ui.wait_for("⏸ Ask before changes").await;
            ui.submit(&format!("/resume {session}"));
            let screen = ui
                .wait("the resumed session", |s| {
                    idle(s, "⏺ hello") && s.contains("⏵⏵ Full access")
                })
                .await;
            assert!(!screen.contains(TITLE), "{screen}");
        },
    )
    .await;
}

/// 文字是 App 的 i18n 原文（`mode.yoloConfirmation.*`、`common.cancel`），跟随界面语言。
#[tokio::test(flavor = "multi_thread")]
async fn the_texts_are_the_app_texts_in_both_languages() {
    let cases = [
        (
            "en_US.UTF-8",
            "⏵⏵ Autonomous mode",
            TITLE,
            [
                "Tasks can perform the following actions without asking for your approval each time.",
                "Files and folders",
                "Read, create, change, and delete accessible files.",
                "Terminal commands",
                "Run commands, install software, or change system settings.",
                "Network and apps",
                "Access the network, send data, and use connected apps.",
                "These actions may cause data loss or disclosure. Enable only for tasks you trust. You can switch to another permission mode at any time.",
                "[ ] I understand the risks and take responsibility for my data security.",
                "1. Cancel",
                "2. Confirm",
                "↑/↓ to navigate · Space to check · Enter to select · Esc to cancel",
            ],
        ),
        (
            "zh_CN.UTF-8",
            "⏵⏵ 自主模式",
            "⚠ 要启用完全访问吗？",
            [
                "启用后，任务可以直接执行以下操作，无需逐次请求你确认。",
                "文件和文件夹",
                "读取、创建、修改和删除可访问的文件。",
                "终端命令",
                "运行命令、安装软件或修改系统配置。",
                "网络和应用",
                "访问网络、发送数据，以及调用已连接的应用。",
                "这些操作可能造成数据丢失或泄露。请仅在信任任务内容时启用。你可以随时切回其他权限模式。",
                "[ ] 我已了解风险，并对自己的数据安全负责。",
                "1. 取消",
                "2. 确认启用",
                "↑/↓ 移动 · space 勾选 · enter 选择 · esc 取消",
            ],
        ),
    ];
    for (lang, guarded, title, texts) in cases {
        let setup = Setup {
            lang,
            height: 30,
            ..Setup::default()
        };
        with_ui(
            GatedModel::scripted(Vec::new()),
            setup,
            |mut ui| async move {
                let screen = open_by_shift_tab(&mut ui, guarded, title).await;
                let all = joined(&screen);
                for text in texts {
                    assert!(all.contains(&joined(text)), "{text}\n{screen}");
                }
            },
        )
        .await;
    }
}

type Rgb = (u8, u8, u8);

/// 一种终端下框里的颜色：App 的 `--color-warning`、`--color-destructive` 与本工程的 `success`。
struct Colors {
    palette: Palette,
    warning: Color,
    destructive: Color,
    success: Color,
}

fn colors() -> [Colors; 4] {
    let truecolor = |background: Rgb| {
        Palette::new(ColorLevel::TrueColor, Some(background)).with_foreground(Some(
            if background == (0, 0, 0) {
                (255, 255, 255)
            } else {
                (0, 0, 0)
            },
        ))
    };
    [
        // 深色：yellow-500、red-500。
        Colors {
            palette: truecolor((0, 0, 0)),
            warning: Color::Rgb(240, 177, 0),
            destructive: Color::Rgb(251, 44, 54),
            success: Color::Rgb(78, 186, 101),
        },
        // 浅色：yellow-600、red-600。
        Colors {
            palette: truecolor((255, 255, 255)),
            warning: Color::Rgb(208, 135, 0),
            destructive: Color::Rgb(231, 0, 11),
            success: Color::Rgb(44, 122, 57),
        },
        // 16 色终端。
        Colors {
            palette: Palette::new(ColorLevel::Ansi16, Some((0, 0, 0))),
            warning: Color::Yellow,
            destructive: Color::Red,
            success: Color::LightGreen,
        },
        Colors {
            palette: Palette::new(ColorLevel::Ansi16, Some((255, 255, 255))),
            warning: Color::Yellow,
            destructive: Color::Red,
            success: Color::Green,
        },
    ]
}

fn frame(ui: &Ui) -> Buffer {
    ui.buffer.lock().unwrap().clone().unwrap()
}

/// 第一处 `needle` 的第一个字所在的（列，行）。
fn find(buffer: &Buffer, needle: &str) -> (u16, u16) {
    let area = buffer.area;
    for y in area.top()..area.bottom() {
        let cells: Vec<(u16, String)> = (area.left()..area.right())
            .map(|x| (x, buffer[(x, y)].symbol().to_owned()))
            .collect();
        let row: String = cells.iter().map(|(_, s)| s.as_str()).collect();
        if let Some(at) = row.find(needle) {
            let mut bytes = 0;
            let (x, _) = cells
                .iter()
                .find(|(_, s)| {
                    let hit = bytes >= at;
                    bytes += s.len();
                    hit
                })
                .unwrap();
            return (*x, y);
        }
    }
    panic!("no {needle} on the screen");
}

fn cell(buffer: &Buffer, needle: &str) -> ratatui::buffer::Cell {
    buffer[find(buffer, needle)].clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_dialog_uses_the_app_colors_in_both_themes_and_16_colors() {
    for colors in colors() {
        let accent = colors.palette.accent;
        let setup = Setup {
            palette: colors.palette,
            ..Setup::default()
        };
        with_ui(
            GatedModel::scripted(Vec::new()),
            setup,
            |mut ui| async move {
                open_by_shift_tab(&mut ui, "⏵⏵ Autonomous mode", TITLE).await;
                let buffer = frame(&ui);
                // 顶上的线与标题：warning，标题加粗。
                let title = cell(&buffer, TITLE);
                assert_eq!(title.fg, colors.warning, "{title:?}");
                assert!(title.modifier.contains(Modifier::BOLD), "{title:?}");
                let (_, row) = find(&buffer, TITLE);
                let rule = &buffer[(0, row - 1)];
                assert_eq!((rule.symbol(), rule.fg), ("─", colors.warning));
                // 焦点在取消：指针与名字是强调色；确认没勾选时 dim。
                assert_eq!(cell(&buffer, "❯ 1. Cancel").fg, accent);
                assert_eq!(cell(&buffer, "Cancel").fg, accent);
                let confirm = cell(&buffer, "Confirm");
                assert!(confirm.modifier.contains(Modifier::DIM), "{confirm:?}");
                // 勾上：方框是 success；确认是 destructive，聚焦时指针仍是强调色。
                ui.key(KeyCode::Up);
                ui.key(KeyCode::Char(' '));
                ui.wait_for("[✔]").await;
                assert_eq!(cell(&frame(&ui), "✔").fg, colors.success);
                ui.key(KeyCode::Down);
                ui.key(KeyCode::Down);
                ui.wait_for("❯ 2. Confirm").await;
                let buffer = frame(&ui);
                let confirm = cell(&buffer, "Confirm");
                assert_eq!(confirm.fg, colors.destructive, "{confirm:?}");
                assert!(!confirm.modifier.contains(Modifier::DIM), "{confirm:?}");
                assert_eq!(cell(&buffer, "❯ 2. Confirm").fg, accent);
            },
        )
        .await;
    }
}

/// 窄终端：每段文字都按宽度折行，整段都在屏幕上（没有被截掉）。
#[tokio::test(flavor = "multi_thread")]
async fn narrow_terminals_wrap_the_text() {
    for (lang, guarded, title, texts) in [
        (
            "en_US.UTF-8",
            "⏵⏵ Autonomous mode",
            "Enable full access?",
            [
                "Tasks can perform the following actions without asking for your approval each time.",
                "Run commands, install software, or change system settings.",
                "These actions may cause data loss or disclosure. Enable only for tasks you trust. You can switch to another permission mode at any time.",
                "I understand the risks and take responsibility for my data security.",
            ],
        ),
        (
            "zh_CN.UTF-8",
            "⏵⏵ 自主模式",
            "要启用完全访问吗？",
            [
                "启用后，任务可以直接执行以下操作，无需逐次请求你确认。",
                "运行命令、安装软件或修改系统配置。",
                "这些操作可能造成数据丢失或泄露。请仅在信任任务内容时启用。你可以随时切回其他权限模式。",
                "我已了解风险，并对自己的数据安全负责。",
            ],
        ),
    ] {
        let setup = Setup {
            lang,
            width: 36,
            height: 44,
            ..Setup::default()
        };
        with_ui(
            GatedModel::scripted(Vec::new()),
            setup,
            |mut ui| async move {
                let screen = open_by_shift_tab(&mut ui, guarded, title).await;
                let all = joined(&screen);
                for text in texts {
                    assert!(all.contains(&joined(text)), "{text}\n{screen}");
                }
                assert!(screen.contains("❯ 1. "), "{screen}");
            },
        )
        .await;
    }
}
