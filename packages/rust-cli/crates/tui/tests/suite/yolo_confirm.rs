//! 切到完全访问的二次确认（docs/specs/rust-cli/tui.md §7.8，工作线 F87，用户决定 2026-10-24：照 App 的确认框，
//! 是局部的小框）：shift+tab、`/mode yolo`、`/mode` 选择器三个入口都先弹框；没勾选时确认不可用，勾选后确认才发
//! 模式命令，取消留在原来的模式；框开着时不叠框；已经是完全访问、启动参数与恢复的会话都不弹；中英文是 App 的原文；
//! 颜色是 App 的 token（深浅两种背景、16 色终端）；窄终端折行。旧代码直接切换，这些测试都失败。样子照 Puffin 的确认框
//! （工作线 TK 的阶段 S，tui-kit.md §4.2）：板上的 `□` / `■` 勾选框与编号的回答，焦点只改颜色（序号与名字 brand）。
use std::sync::Arc;

use super::support::Shot;
use crossterm::event::{KeyCode, KeyModifiers};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::store::{Fact, LoadedSession};
use rust_cli_kernel_api::testing::{MemoryStore, StoreHooks, StoreProxy, text_response};
use rust_cli_tui::{ColorLevel, Colors};
use rust_cli_tui_kit::render::Color;

use super::support::{GatedModel, Setup, Ui, row_of, with_ui};

const TITLE: &str = "⚠ Enable full access?";
/// 已经是完全访问时 `/mode` 的入口说的一句（`i18n::mode_says`）。
const SAYS: &str = "Full access: it runs commands and edits files without asking.";

/// 回答成了一行（Puffin 的回答没有记号）且回合结束。
fn idle(screen: &str, text: &str) -> bool {
    screen.lines().any(|line| line.trim() == text) && !screen.contains("esc stop")
}

/// 缺省终端（`Setup::default` 的墨色）的 brand：聚焦的回答行的序号与名字。
fn brand() -> Color {
    Setup::default().colors.ink().theme.brand
}

/// 等到 `label` 的第一个字是 brand（聚焦；焦点移动只改颜色，画面的文字不变）。等待有上限，只为让失败的测试结束。
async fn until_focused(ui: &Ui, label: &str, brand: Color) {
    let wait = async {
        loop {
            let shot = ui.frame();
            if shot.find(label).is_some_and(|at| shot[at].fg == brand) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    };
    if tokio::time::timeout(std::time::Duration::from_secs(10), wait)
        .await
        .is_err()
    {
        panic!(
            "timed out waiting for the focus on {label}; screen:\n{}",
            ui.screen()
        );
    }
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
        ui.wait("the reply", |s| idle(s, "hello")).await;
        let screen = open_by_shift_tab(&mut ui, "Autonomous", TITLE).await;
        // 局部的框：上面的对话照常显示，输入框让位；焦点在取消。
        assert!(
            screen.lines().any(|line| line.trim() == "hello"),
            "{screen}"
        );
        assert!(screen.contains("   □ I understand the risks"), "{screen}");
        assert!(screen.contains("   1  Cancel"), "{screen}");
        assert!(screen.contains("   2  Confirm"), "{screen}");
        // 80×24 放得下整个框：板的上沿没有被截掉（16 色终端的板是 `─` 线）。
        let title = row_of(&screen, TITLE).unwrap() as usize;
        assert!(
            screen.lines().nth(title - 1).unwrap().starts_with("────"),
            "{screen}"
        );
        // 没勾选时确认不可用：enter 不起作用，框还在（接着按 ↓ 回绕到勾选框）。
        ui.key(KeyCode::Down);
        until_focused(&ui, "Confirm", brand()).await;
        ui.key(KeyCode::Enter);
        ui.key(KeyCode::Down);
        until_focused(&ui, "I understand the risks", brand()).await;
        // esc 取消：留在自主模式。
        ui.key(KeyCode::Esc);
        let screen = ui.wait("the dialog to close", |s| !s.contains(TITLE)).await;
        assert!(screen.contains("Autonomous"), "{screen}");
        // 再打开：勾选只属于上一次，这次又没勾；勾上之后确认才切换。
        shift_tab(&ui);
        let screen = ui.wait_for(TITLE).await;
        assert!(screen.contains("   □ I understand the risks"), "{screen}");
        ui.key(KeyCode::Up);
        ui.key(KeyCode::Char(' '));
        ui.wait_for("■ I understand the risks").await;
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Down);
        until_focused(&ui, "Confirm", brand()).await;
        ui.key(KeyCode::Enter);
        let screen = ui
            .wait("full access", |s| {
                !s.contains(TITLE) && s.contains("Full access")
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
        ui.wait_for("Ask before changes").await;
        ui.submit("/mode yolo");
        ui.wait_for(TITLE).await;
        ui.key(KeyCode::Esc);
        let screen = ui.wait("the dialog to close", |s| !s.contains(TITLE)).await;
        assert!(screen.contains("Ask before changes"), "{screen}");
        assert!(!screen.contains("Mode switched"), "{screen}");
        // 选择器：数字键选完全访问也先问；ctrl+c 同取消。
        // 选择器（S3 的 Puffin 列表，tui-kit.md §4.2）：数字键照旧。
        ui.submit("/mode");
        // `/mode` 的层没有头一行（Puffin）：等它的条目。
        ui.wait_for("Asks only before commands that can destroy work")
            .await;
        ui.key(KeyCode::Char('3'));
        ui.wait_for(TITLE).await;
        ui.press(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let screen = ui.wait("the dialog to close", |s| !s.contains(TITLE)).await;
        assert!(screen.contains("Ask before changes"), "{screen}");
        assert!(!screen.contains("Mode switched"), "{screen}");
        // 勾选框上 enter 同 space；确认之后才提示并切换。
        ui.submit("/mode YOLO");
        ui.wait_for(TITLE).await;
        ui.key(KeyCode::Up);
        ui.key(KeyCode::Enter);
        ui.wait_for("■ I understand the risks").await;
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Enter);
        // 确认之后切换，输入框上方说模式的那一句（Puffin 的 `asked`：模式每次变了都说，`status_motion.rs`）。
        let screen = ui
            .wait("full access", |s| {
                !s.contains(TITLE) && s.contains("Full access ⇧⇥") && s.contains(SAYS)
            })
            .await;
        assert!(screen.contains(SAYS), "{screen}");
        // 已经是完全访问：两个入口都不再问，模式没变也不说那一句（Puffin 的 `asked` 只在模式变了时说）；输入框上方的
        // 那一句发送时清掉（tui-kit.md §4.0）。
        ui.submit("/mode yolo");
        ui.settle().await;
        let screen = ui.screen();
        assert!(
            !screen.contains(TITLE) && !screen.contains(SAYS),
            "{screen}"
        );
        ui.submit("/mode");
        ui.wait("the picker with full access current", |s| {
            s.lines().any(|l| {
                l.trim_start().starts_with("Full access") && l.trim_end().ends_with("current")
            })
        })
        .await;
        ui.key(KeyCode::Char('3'));
        ui.wait("the picker closes", |s| {
            !s.contains("Asks only before commands")
        })
        .await;
        ui.settle().await;
        let screen = ui.screen();
        assert!(
            !screen.contains(TITLE) && !screen.contains(SAYS),
            "{screen}"
        );
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
            ["Autonomous", TITLE, "Cancel", "Confirm"],
            ["I understand the risks", "Full access"],
        ),
        (
            "zh_CN.UTF-8",
            ["自主模式", "⚠ 要启用完全访问吗？", "取消", "确认启用"],
            ["我已了解风险", "完全访问"],
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
                until_focused(&ui, confirm, brand()).await;
                let screen = ui.screen();
                assert!(screen.contains(&format!("   □ {ack}")), "{screen}");
                assert!(screen.contains(title), "{screen}");
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
                let screen = ui.wait_for(&format!("■ {ack}")).await;
                assert_eq!(screen.matches(title).count(), 1, "{screen}");
                assert!(screen.contains(&format!("   1  {cancel}")), "{screen}");
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
        ui.wait_for("Ask before changes").await;
        open_by_shift_tab(&mut ui, "Autonomous", TITLE).await;
        ui.key(KeyCode::Up);
        ui.key(KeyCode::Char(' '));
        ui.wait_for("■ I understand the risks").await;
        shift_tab(&ui);
        shift_tab(&ui);
        // 焦点仍在勾选框上：↓ 到取消（重新打开的框焦点在取消，↓ 会到确认）。
        ui.key(KeyCode::Down);
        until_focused(&ui, "Cancel", brand()).await;
        let screen = ui.screen();
        assert_eq!(screen.matches(TITLE).count(), 1, "{screen}");
        assert!(screen.contains("   ■ I understand the risks"), "{screen}");
        ui.key(KeyCode::Esc);
        let screen = ui.wait("the dialog to close", |s| !s.contains(TITLE)).await;
        assert!(screen.contains("Autonomous"), "{screen}");
    })
    .await;
}

/// 冷加载时像 node-store 一样读回会话最后提交的权限模式（内存存储的替身不读回，`load` 的 `mode` 总是 None）。
struct ModeOnLoad(Arc<MemoryStore>);

impl StoreHooks for ModeOnLoad {
    fn loaded(&self, session: &SessionId, loaded: &mut LoadedSession) {
        loaded.mode = committed_modes(&self.0, session.as_str()).last().copied();
    }
}

fn mode_on_load(store: &Arc<MemoryStore>) -> Arc<StoreProxy<ModeOnLoad>> {
    Arc::new(StoreProxy::new(store.clone(), ModeOnLoad(store.clone())))
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
        let screen = ui.wait_for("Full access").await;
        assert!(!screen.contains(TITLE), "{screen}");
        ui.submit("hi");
        let screen = ui.wait("the reply", |s| idle(s, "hello")).await;
        assert!(!screen.contains(TITLE), "{screen}");
    })
    .await
    .unwrap();
    // `--resume <id>`：启动参数是缺省模式，会话读回来是完全访问。
    let setup = Setup {
        session_store: Some(mode_on_load(&store)),
        startup: rust_cli_tui::Startup::Session(session.clone()),
        ..Setup::default()
    };
    with_ui(
        GatedModel::scripted(Vec::new()),
        setup,
        |mut ui| async move {
            let screen = ui.wait("the resumed session", |s| idle(s, "hello")).await;
            assert!(screen.contains("Full access"), "{screen}");
            assert!(!screen.contains(TITLE), "{screen}");
        },
    )
    .await;
    // 会话里 `/resume <id>`。
    let setup = Setup {
        session_store: Some(mode_on_load(&store)),
        ..Setup::default()
    };
    with_ui(
        GatedModel::scripted(Vec::new()),
        setup,
        |mut ui| async move {
            ui.wait_for("Ask before changes").await;
            ui.submit(&format!("/resume {session}"));
            let screen = ui
                .wait("the resumed session", |s| {
                    idle(s, "hello") && s.contains("Full access")
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
            "Autonomous",
            TITLE,
            [
                "Tasks can perform the following actions without asking for your approval each time.",
                "Files and folders",
                "Read, create, change, and delete accessible files.",
                "Terminal commands",
                // 三项一行一项（Puffin），80 列时这一项的末尾以 `…` 截断。
                "Run commands, install software, or change system",
                "Network and apps",
                "Access the network, send data, and use connected apps.",
                "These actions may cause data loss or disclosure. Enable only for tasks you trust. You can switch to another permission mode at any time.",
                "□ I understand the risks and take responsibility for my data security.",
                "1  Cancel",
                "2  Confirm",
                "1–2 answer  esc cancel",
            ],
        ),
        (
            "zh_CN.UTF-8",
            "自主模式",
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
                "□ 我已了解风险，并对自己的数据安全负责。",
                "1  取消",
                "2  确认启用",
                "1–2 回答  esc 取消",
            ],
        ),
    ];
    for (lang, guarded, title, texts) in cases {
        // 按键提示在槽里：放不下最后一个回答的右沿时不画（Puffin `on_row`，tui-kit.md §4.0「板」），这里给够宽度。
        let setup = Setup {
            lang,
            width: 120,
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

/// 一种终端下框里的颜色：Puffin 确认框的主题 token（tui-kit.md §3.4、§4.2）。`⚠` 是 `code`，聚焦的回答 brand，没勾选的
/// “确认”是 `line`（不可用），勾选后不聚焦时是 `err`（破坏性），勾上的 `■` 是 brand。
struct Marks {
    colors: Colors,
    code: Color,
    err: Color,
    brand: Color,
    line: Color,
}

fn colors() -> [Marks; 4] {
    let truecolor = |background: Rgb| {
        Colors::new(ColorLevel::TrueColor, Some(background)).with_foreground(Some(
            if background == (0, 0, 0) {
                (255, 255, 255)
            } else {
                (0, 0, 0)
            },
        ))
    };
    let of = |colors: Colors| {
        let theme = colors.ink().theme;
        Marks {
            colors,
            code: theme.code,
            err: theme.err,
            brand: theme.brand,
            line: theme.line.fg_color(),
        }
    };
    [
        // 深色、浅色、16 色终端的两种背景。
        of(truecolor((0, 0, 0))),
        of(truecolor((255, 255, 255))),
        of(Colors::new(ColorLevel::Ansi16, Some((0, 0, 0)))),
        of(Colors::new(ColorLevel::Ansi16, Some((255, 255, 255)))),
    ]
}

fn frame(ui: &Ui) -> Shot {
    ui.frame()
}

/// 第一处 `needle` 的第一个字所在的（列，行）。
fn find(buffer: &Shot, needle: &str) -> (u16, u16) {
    buffer
        .find(needle)
        .unwrap_or_else(|| panic!("no {needle} on the screen"))
}

fn cell(buffer: &Shot, needle: &str) -> super::support::ShotCell {
    buffer[find(buffer, needle)].clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_dialog_uses_the_app_colors_in_both_themes_and_16_colors() {
    for colors in colors() {
        let brand = colors.brand;
        let setup = Setup {
            colors: colors.colors,
            ..Setup::default()
        };
        with_ui(
            GatedModel::scripted(Vec::new()),
            setup,
            |mut ui| async move {
                open_by_shift_tab(&mut ui, "Autonomous", TITLE).await;
                let buffer = frame(&ui);
                // 标题前的 `⚠` 是 amber（`code`）；板的上沿在标题的上一行（rich 是 `▄`，16 色是 `─`）。
                assert_eq!(cell(&buffer, "⚠").fg, colors.code);
                let (_, row) = find(&buffer, TITLE);
                let edge = buffer[(0, row - 1)].symbol().to_owned();
                assert!(edge == "▄" || edge == "─", "{edge:?}");
                // 焦点在取消：序号与名字是 brand；确认没勾选时是 line 色（不可用）。
                assert_eq!(cell(&buffer, "1  Cancel").fg, brand);
                assert_eq!(cell(&buffer, "Cancel").fg, brand);
                assert_eq!(cell(&buffer, "Confirm").fg, colors.line);
                // 勾上：`■` 是 brand；不聚焦的确认是 err，聚焦时是 brand。
                ui.key(KeyCode::Up);
                ui.key(KeyCode::Char(' '));
                ui.wait_for("■").await;
                assert_eq!(cell(&frame(&ui), "■").fg, brand);
                ui.key(KeyCode::Down);
                until_focused(&ui, "Cancel", brand).await;
                assert_eq!(cell(&frame(&ui), "Confirm").fg, colors.err);
                ui.key(KeyCode::Down);
                until_focused(&ui, "Confirm", brand).await;
                assert_eq!(cell(&frame(&ui), "2  Confirm").fg, brand);
            },
        )
        .await;
    }
}

/// 窄终端：说明、风险段与勾选框的文字按宽度折行，整段都在屏幕上（三项是 Puffin 的一行一项，放不下时以 `…` 截断，
/// 不在这里比）。
#[tokio::test(flavor = "multi_thread")]
async fn narrow_terminals_wrap_the_text() {
    for (lang, guarded, title, texts) in [
        (
            "en_US.UTF-8",
            "Autonomous",
            "Enable full access?",
            [
                "Tasks can perform the following actions without asking for your approval each time.",
                "These actions may cause data loss or disclosure. Enable only for tasks you trust. You can switch to another permission mode at any time.",
                "I understand the risks and take responsibility for my data security.",
                "1  Cancel",
            ],
        ),
        (
            "zh_CN.UTF-8",
            "自主模式",
            "要启用完全访问吗？",
            [
                "启用后，任务可以直接执行以下操作，无需逐次请求你确认。",
                "这些操作可能造成数据丢失或泄露。请仅在信任任务内容时启用。你可以随时切回其他权限模式。",
                "我已了解风险，并对自己的数据安全负责。",
                "1  取消",
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
            },
        )
        .await;
    }
}
