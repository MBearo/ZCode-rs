//! 远端执行 R4（docs/specs/rust-cli/remote-exec.md §21.5、§21.6）：工具行的 `@机器`（不并进工作段）、询问框写明机器、
//! 状态行与列表在深浅主题与 16 色终端下的颜色。远端机器是 kernel-api 的 `TestMachines`（“远端”也是替身），机器服务是
//! `machines.rs` 的 `FakeMachines`。旧代码的工具行把 `machine` 写进参数摘要、Bash 折成 `Ran 1 shell command`，询问框
//! 不提机器，这些测试都失败。
use std::sync::Arc;

use super::support::Shot;
use crossterm::event::KeyCode;
use rust_cli_kernel_api::env_registry::EnvironmentRegistry;
use rust_cli_kernel_api::testing::{
    TestMachines, UnusedEnvironment, text_response, tool_use_response,
};
use rust_cli_tui::machines::{MachineFailure, MachineStatus};
use rust_cli_tui::{ColorLevel, Colors, Rgb};
use rust_cli_tui_kit::render::Color;
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

/// 工具行（§21.6）：问着时是 `Bash  touch remote.txt  @dev-a`，`machine` 不在参数里；询问框写明机器。
#[tokio::test(flavor = "multi_thread")]
async fn a_remote_call_names_its_machine_on_the_row_and_in_the_ask() {
    for (lang, ask, done) in [
        // 询问框里机器在板上面的轨上（tui-kit.md §6「请求的头行」）。
        ("en_US.UTF-8", "│ runs on dev-a", "remote step done"),
        ("zh_CN.UTF-8", "│ 在 dev-a 上执行", "remote step done"),
    ] {
        let model = remote_bash();
        with_ui(model.clone(), remote_setup(lang), |mut ui| async move {
            ui.submit("make a remote file");
            let screen = ui
                .wait(ask, |s| s.contains("touch remote.txt") && s.contains(ask))
                .await;
            assert!(!screen.contains("machine:"), "{screen}");
            // 问着时调用是一行，行尾是机器（kit `tool_row` 的 `@dev-a`）。
            assert!(
                screen.contains("Bash  touch remote.txt  @dev-a"),
                "{screen}"
            );
            ui.settle().await;
            ui.key(KeyCode::Enter);
            // 结束之后这一段工作收成一句（Puffin 的收拢；替身的执行环境报错，是 `✗ Bash failed`）。
            let screen = ui.wait_for(done).await;
            assert!(!screen.contains("Ran 1 shell command"), "{screen}");
            assert!(!screen.contains("运行了 1 条"), "{screen}");
        })
        .await;
        // 允许后在 dev-a 上执行（替身的执行环境报错，不是拒绝）。
        let second = super::support::request_text(&model.requests()[1]);
        assert!(!second.contains("doesn't want to proceed"), "{second}");
    }
}

/// 一种终端下的颜色：成功、警告、破坏性三种记号与强调色。改由 `Ink` 的角色给（tui-kit.md §3.4）：`ok`、`code`、
/// `err` 与 `brand`；期望值经桥求。
struct Marks {
    colors: Colors,
    success: Color,
    warning: Color,
    destructive: Color,
    accent: Color,
}

fn colors() -> [Marks; 4] {
    let truecolor = |background: Rgb, foreground: Rgb| {
        Colors::new(ColorLevel::TrueColor, Some(background)).with_foreground(Some(foreground))
    };
    let of = |colors: Colors| {
        let ink = colors.ink();
        let color = |c| c;
        Marks {
            colors,
            success: color(ink.success().fg_color()),
            warning: color(ink.warning().fg_color()),
            destructive: color(ink.destructive().fg_color()),
            accent: color(ink.accent()),
        }
    };
    [
        of(truecolor((0, 0, 0), (255, 255, 255))),
        of(truecolor((255, 255, 255), (0, 0, 0))),
        of(Colors::new(ColorLevel::Ansi16, Some((0, 0, 0)))),
        of(Colors::new(ColorLevel::Ansi16, Some((255, 255, 255)))),
    ]
}

fn frame(ui: &Ui) -> Shot {
    ui.frame()
}

/// 第一处 `needle` 的第一个字所在的（列，行）。
fn find(buffer: &Shot, needle: &str) -> (u16, u16) {
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

fn fg(buffer: &Shot, needle: &str) -> Color {
    buffer[find(buffer, needle)].fg
}

/// 状态行与列表的颜色（§21.5、§21.3）：本机与就绪的 `●` 是 success，重连中 `○` 是 warning，失败的 `✗` 与说明是
/// destructive（S3 起照 Puffin 的记号，tui-kit.md §4.2「`/machines`」）；列表的标题是 fg2（Puffin 的头一行）；深浅主题与
/// 16 色终端各一种。
#[tokio::test(flavor = "multi_thread")]
async fn machine_states_use_the_tokens_in_both_themes_and_16_colors() {
    for colors in colors() {
        let rows = vec![
            row("dev-a", MachineStatus::Ready),
            row(
                "gpu-1",
                MachineStatus::Reconnecting {
                    attempt: 1,
                    retry_at_ms: None,
                    reason: String::new(),
                },
            ),
            row(
                "ci-3",
                MachineStatus::Failed(MachineFailure::Lost("gone".to_owned())),
            ),
        ];
        let mut setup = setup_with(Some(FakeMachines::with(rows)), "en_US.UTF-8");
        setup.colors = colors.colors;
        let title = setup.colors.ink().theme.fg2.fg_color();
        with_ui(
            GatedModel::scripted(Vec::new()),
            setup,
            |mut ui| async move {
                // 坞里只有在变的机器（tui-kit.md §4.0 第 6 条）；就绪的在 `/machines` 的列表里。
                ui.wait_for("✗ ci-3").await;
                let buffer = frame(&ui);
                assert_eq!(fg(&buffer, "○ gpu-1"), colors.warning);
                assert_eq!(fg(&buffer, "✗ ci-3"), colors.destructive);
                ui.submit("/machines");
                ui.wait_for("1 of 3 connected").await;
                assert_eq!(fg(&frame(&ui), "● dev-a"), colors.success);
                assert_eq!(fg(&frame(&ui), "Machines"), title);
                // 失败的原因在选中那台的下面（Puffin 的细节行，`cause` 把原因写成一句）。
                ui.key(KeyCode::Down);
                ui.key(KeyCode::Down);
                ui.wait_for("     gone.").await;
                let buffer = frame(&ui);
                assert_eq!(fg(&buffer, "gone."), colors.destructive);
            },
        )
        .await;
    }
}

/// 询问框里的机器用强调色（§21.6）；工具行尾的 `@机器` 是 Puffin 的墨（`@` fg3、名字 fg2）。深浅主题各一种。
#[tokio::test(flavor = "multi_thread")]
async fn the_machine_tag_takes_the_accent_in_the_ask_and_puffin_inks_on_the_row() {
    for colors in colors().into_iter().take(2) {
        let mut setup = remote_setup("en_US.UTF-8");
        setup.colors = colors.colors;
        // 强调色经 `Ink`（tui-kit.md §3.4、§3.7：`brand`），`colors()` 经桥求好。
        let accent = colors.accent;
        let setup_theme = setup.colors.ink();
        with_ui(remote_bash(), setup, |mut ui| async move {
            ui.submit("make a remote file");
            ui.wait_for("runs on dev-a").await;
            // 预览的轨上 `runs on` 后面的机器名（行尾的 `@dev-a` 在它上面，是 Puffin 的墨）。
            let shot = frame(&ui);
            let (x, y) = find(&shot, "runs on dev-a");
            assert_eq!(shot[(x + 8, y)].fg, accent);
            // 工具行尾的机器是 kit `tool_row` 的 `@`（fg3）与名字（fg2），同 Puffin（以前整个 `@dev-a` 是强调色）。回合结束后
            // 这一段工作收成一句，所以在问着时看。
            let theme = setup_theme.theme;
            let (x, y) = find(&shot, "remote.txt  @dev-a");
            let at = x + "remote.txt  ".len() as u16;
            assert_eq!(shot[(at, y)].fg, theme.fg3.fg_color());
            assert_eq!(shot[(at + 1, y)].fg, theme.fg2.fg_color());
            ui.settle().await;
            ui.key(KeyCode::Enter);
            ui.wait_for("remote step done").await;
        })
        .await;
    }
}
