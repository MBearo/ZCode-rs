//! 远端执行 R4（docs/specs/rust-cli/remote-exec.md §21.5、§21.6）：工具行的 `@机器`（不并进工作段）、询问框写明机器、
//! 状态行与列表在深浅主题与 16 色终端下的颜色。远端机器是 kernel-api 的 `TestMachines`（“远端”也是替身），机器服务是
//! `machines.rs` 的 `FakeMachines`。旧代码的工具行把 `machine` 写进参数摘要、Bash 折成 `Ran 1 shell command`，询问框
//! 不提机器，这些测试都失败。
use std::sync::Arc;

use crossterm::event::KeyCode;
use ratatui::buffer::Buffer;
use ratatui::style::Color;
use rust_cli_kernel_api::env_registry::EnvironmentRegistry;
use rust_cli_kernel_api::testing::{
    TestMachines, UnusedEnvironment, text_response, tool_use_response,
};
use rust_cli_tui::machines::{MachineFailure, MachineStatus};
use rust_cli_tui::ui::palette::{ColorLevel, Palette, Rgb};
use serde_json::json;

use super::machines::{FakeMachines, row, setup_with};
use super::support::{GatedModel, Setup, Ui, with_ui};

fn remote_setup(lang: &'static str) -> Setup {
    let machines = TestMachines::new(Arc::new(UnusedEnvironment));
    machines.connect("dev-a", Arc::new(UnusedEnvironment), "/srv/proj");
    Setup {
        width: 100,
        height: 30,
        lang,
        environments: machines as Arc<dyn EnvironmentRegistry>,
        ..Setup::default()
    }
}

fn remote_bash() -> Arc<GatedModel> {
    GatedModel::scripted(vec![
        tool_use_response(
            "toolu_1",
            "Bash",
            &json!({"command": "touch remote.txt", "description": "make a file", "machine": "dev-a"}),
        ),
        text_response(&["remote step done"]),
    ])
}

/// 工具行（§21.6）：`⏺ Bash(touch remote.txt)  @dev-a`，`machine` 不在参数里，不折进工作段；询问框写明机器。
#[tokio::test(flavor = "multi_thread")]
async fn a_remote_call_names_its_machine_on_the_row_and_in_the_ask() {
    for (lang, ask, done) in [
        ("en_US.UTF-8", "· runs on dev-a", "remote step done"),
        ("zh_CN.UTF-8", "· 在 dev-a 上执行", "remote step done"),
    ] {
        let model = remote_bash();
        with_ui(model.clone(), remote_setup(lang), |mut ui| async move {
            ui.submit("make a remote file");
            let screen = ui
                .wait(ask, |s| s.contains("touch remote.txt") && s.contains(ask))
                .await;
            assert!(!screen.contains("machine:"), "{screen}");
            ui.settle().await;
            ui.key(KeyCode::Enter);
            let screen = ui.wait_for(done).await;
            assert!(
                screen.contains("Bash(touch remote.txt)  @dev-a"),
                "{screen}"
            );
            assert!(!screen.contains("Ran 1 shell command"), "{screen}");
            assert!(!screen.contains("运行了 1 条"), "{screen}");
        })
        .await;
        // 允许后在 dev-a 上执行（替身的执行环境报错，不是拒绝）。
        let second = super::support::request_text(&model.requests()[1]);
        assert!(!second.contains("doesn't want to proceed"), "{second}");
    }
}

/// 一种终端下的颜色：成功、警告、破坏性三种记号与强调色。
struct Colors {
    palette: Palette,
    success: Color,
    warning: Color,
    destructive: Color,
}

fn colors() -> [Colors; 4] {
    let truecolor = |background: Rgb, foreground: Rgb| {
        Palette::new(ColorLevel::TrueColor, Some(background)).with_foreground(Some(foreground))
    };
    [
        Colors {
            palette: truecolor((0, 0, 0), (255, 255, 255)),
            success: Color::Rgb(78, 186, 101),
            warning: Color::Rgb(240, 177, 0),
            destructive: Color::Rgb(251, 44, 54),
        },
        Colors {
            palette: truecolor((255, 255, 255), (0, 0, 0)),
            success: Color::Rgb(44, 122, 57),
            warning: Color::Rgb(208, 135, 0),
            destructive: Color::Rgb(231, 0, 11),
        },
        Colors {
            palette: Palette::new(ColorLevel::Ansi16, Some((0, 0, 0))),
            success: Color::LightGreen,
            warning: Color::Yellow,
            destructive: Color::Red,
        },
        Colors {
            palette: Palette::new(ColorLevel::Ansi16, Some((255, 255, 255))),
            success: Color::Green,
            warning: Color::Yellow,
            destructive: Color::Red,
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
        let line: String = cells.iter().map(|(_, s)| s.as_str()).collect();
        if let Some(at) = line.find(needle) {
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

fn fg(buffer: &Buffer, needle: &str) -> Color {
    buffer[find(buffer, needle)].fg
}

/// 状态行与列表的颜色（§21.5、§21.3）：本机与就绪的 `●` 是 success，重连中 `○` 是 warning，失败的 `○` 与说明是
/// destructive；深浅主题与 16 色终端各一种。
#[tokio::test(flavor = "multi_thread")]
async fn machine_states_use_the_tokens_in_both_themes_and_16_colors() {
    for colors in colors() {
        let rows = vec![
            row("dev-a", MachineStatus::Ready),
            row("gpu-1", MachineStatus::Reconnecting { attempt: 1 }),
            row(
                "ci-3",
                MachineStatus::Failed(MachineFailure::Lost("gone".to_owned())),
            ),
        ];
        let mut setup = setup_with(Some(FakeMachines::with(rows)), "en_US.UTF-8");
        setup.palette = colors.palette;
        let accent = colors.palette.accent;
        with_ui(
            GatedModel::scripted(Vec::new()),
            setup,
            |mut ui| async move {
                ui.wait_for("● local  ● dev-a 12ms").await;
                let buffer = frame(&ui);
                assert_eq!(fg(&buffer, "● local"), colors.success);
                assert_eq!(fg(&buffer, "● dev-a"), colors.success);
                assert_eq!(fg(&buffer, "○ gpu-1"), colors.warning);
                assert_eq!(fg(&buffer, "○ ci-3"), colors.destructive);
                ui.submit("/machines");
                ui.wait_for("Lost the connection to ci-3: gone").await;
                let buffer = frame(&ui);
                assert_eq!(fg(&buffer, "Lost the connection"), colors.destructive);
                assert_eq!(fg(&buffer, "Machines"), accent);
            },
        )
        .await;
    }
}

/// `@机器` 用强调色（§21.6），深浅主题各一种。
#[tokio::test(flavor = "multi_thread")]
async fn the_machine_tag_uses_the_accent() {
    for colors in colors().into_iter().take(2) {
        let mut setup = remote_setup("en_US.UTF-8");
        setup.palette = colors.palette;
        let accent = colors.palette.accent;
        with_ui(remote_bash(), setup, |mut ui| async move {
            ui.submit("make a remote file");
            ui.wait_for("runs on dev-a").await;
            assert_eq!(fg(&frame(&ui), "dev-a"), accent);
            ui.settle().await;
            ui.key(KeyCode::Enter);
            ui.wait_for("remote step done").await;
            assert_eq!(fg(&frame(&ui), "@dev-a"), accent);
        })
        .await;
    }
}
