//! 输入框下面的子代理列表与只读查看（docs/specs/rust-cli/tui-panels.md §1）：内核是 LocalKernel（agents 的
//! Agent 工具），列表来自 `subagents()`，查看的内容来自子会话的存储。
use super::agents_tree::{list, open_list};
use super::support::Shot;
use crossterm::event::KeyCode;
use rust_cli_kernel_api::testing::{text_response, tool_use_response};
use rust_cli_tui::{ColorLevel, Colors};
use rust_cli_tui_kit::render::Color;
use serde_json::json;

use super::support::{GatedModel, Setup, Ui, finish, row_of, send, with_ui};

/// 修复线 F62（tui-panels.md §1.6）：查看子代理时输入框的边线与描述的颜色。改由 `Ink::subagent_ink` 给（`agent`
/// token，tui-kit.md §3.4），期望值经桥求；设置报告了前景与背景，主题是 rich。
fn agent_color() -> Color {
    let colors = tall().colors;
    colors.ink().subagent_ink().fg_color()
}

fn frame(ui: &Ui) -> Shot {
    ui.frame()
}

/// 输入行（` › ` 与占位文字那一行）与板上面写描述的那一行（S2 的输入框，板的上沿在两者之间；tui-kit.md §4.3）。
fn input_rows(screen: &str, placeholder: &str) -> (u16, u16) {
    let row = row_of(screen, placeholder)
        .unwrap_or_else(|| panic!("no input row with {placeholder}:\n{screen}"));
    (row - 2, row)
}

fn tall() -> Setup {
    Setup {
        height: 32,
        // 前台子代理只在 fork 关着时有（`ZCODE_FORK_SUBAGENT=0`，修复线 L-A，agents.md §8.1.1）。
        fork_subagent: Some(false),
        colors: Colors::new(ColorLevel::TrueColor, Some((0, 0, 0)))
            .with_foreground(Some((255, 255, 255))),
        ..Setup::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_list_opens_a_read_only_view_of_the_subagent() {
    let (model, senders) = GatedModel::new(3);
    let mut senders = senders.into_iter();
    let (main, child, last) = (
        senders.next().unwrap(),
        senders.next().unwrap(),
        senders.next().unwrap(),
    );
    with_ui(model, tall(), |mut ui| async move {
        ui.submit("find the callers");
        finish(
            main,
            tool_use_response(
                "toolu_agent",
                "Agent",
                &json!({"description": "Find callers", "prompt": "Look around the code",
                        "subagent_type": "Explore", "run_in_background": false}),
            ),
        );
        // 输入框为空时 ↓ 打开板上的层（照 Puffin 的 picker，tui-kit.md §4.3）：主会话（正在看，fg3 的 `current`）与子代理
        // 一行（记号、类型、描述、用时）。transcript 里的工具行的记号在第 1 列，层的在第 3 列。
        open_list(&ui).await;
        ui.wait("the agent list", |s| {
            list(s)
                .iter()
                .any(|l| l.starts_with("     main") && l.ends_with("  current"))
                && list(s)
                    .iter()
                    .any(|l| l.starts_with("   ○ Explore  Find callers"))
        })
        .await;
        // 打开时就选中了子代理（tui-tasks.md §3.2），enter 查看。选中只改颜色（名字 brand，没有指针）：等这一帧。
        let brand = tall().colors.ink().theme.brand;
        let wait = async {
            loop {
                let shot = frame(&ui);
                let row = (0..shot.area.height)
                    .rev()
                    .find(|y| shot.row(*y).starts_with("   ○ Explore"));
                if row.is_some_and(|y| shot[(5, y)].fg == brand) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(10), wait)
            .await
            .unwrap_or_else(|_| panic!("the selection on Explore:\n{}", ui.screen()));
        ui.key(KeyCode::Enter);
        ui.wait_for("↳ Explore · Find callers").await;
        // 卡留着（tui-tasks.md §3.3）：esc 关上它，再看输入框。
        ui.key(KeyCode::Esc);
        let screen = ui
            .wait("the card to close", |s| {
                list(s).is_empty() && s.contains("↳ Explore · Find callers")
            })
            .await;
        assert!(screen.contains("Look around the code"), "{screen}");
        // 修复线 F62：占位文字写 `@类型` 并写明只读；输入行的 `›` 是子代理色，板上面那一行（会话标题的位置）用同色的字
        // 写描述，板的底色带一点子代理色（tui-panels.md §1.6）。
        let placeholder = "Viewing @Explore (read-only) · esc to return";
        assert!(screen.contains(placeholder), "{screen}");
        let (caption, input) = input_rows(&screen, placeholder);
        let buffer = frame(&ui);
        let agent = agent_color();
        assert_eq!(buffer[(1, input)].symbol(), "›");
        assert_eq!(buffer[(1, input)].fg, agent);
        assert!(
            buffer.row(caption).trim_end().ends_with("Find callers"),
            "{screen}"
        );
        // 这一行都是 ASCII：字节位置就是列。
        let x = buffer.row(caption).find("Find callers").unwrap() as u16;
        assert_eq!(buffer[(x, caption)].fg, agent, "{screen}");
        assert_eq!(
            Some(buffer[(1, input)].bg),
            tall().colors.ink().subagent_ground(),
            "{screen}"
        );
        // 输入框只读：打字不生效。
        ui.type_text("hello");
        // 子会话提交了回复：查看中的视图随之更新。
        let mut reply = text_response(&["Checking the files"]);
        let tail = reply.split_off(3);
        send(&child, reply);
        finish(child, tail);
        let screen = ui
            .wait("the child's reply", |s| {
                s.contains("Checking the files") && s.contains("↳ Explore")
            })
            .await;
        assert!(!screen.contains("hello"), "{screen}");
        finish(last, text_response(&["callers found"]));
        // esc 回到主会话：输入框恢复原样（边线不再是子代理色，没有描述的标签）。
        ui.key(KeyCode::Esc);
        let screen = ui.wait_for("callers found").await;
        // transcript 里的工具行收成一段（Puffin 的收拢：`✓ Ran 1 subagent`）。
        assert!(
            screen.lines().any(|l| l.starts_with(" ✓ Ran 1 subagent")),
            "{screen}"
        );
        assert!(!screen.contains("↳ Explore"), "{screen}");
        assert!(!screen.contains("Viewing @Explore"), "{screen}");
        let buffer = frame(&ui);
        // 输入行是画面上最后一个第 1 列是 `›` 的行（上面 transcript 里的用户消息也是）：不再是子代理色。
        let row = (0..buffer.area.height)
            .rev()
            .find(|y| buffer[(1, *y)].symbol() == "›")
            .unwrap();
        assert_ne!(buffer[(1, row)].fg, agent);
        let width = buffer.area.width;
        assert!((0..width).all(|x| buffer[(x, row - 2)].bg != agent));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn chinese_shows_the_main_line_and_the_read_only_placeholder() {
    let (model, senders) = GatedModel::new(3);
    let mut senders = senders.into_iter();
    let (main, child, last) = (
        senders.next().unwrap(),
        senders.next().unwrap(),
        senders.next().unwrap(),
    );
    let setup = Setup {
        lang: "zh_CN.UTF-8",
        ..tall()
    };
    with_ui(model, setup, |mut ui| async move {
        ui.submit("找调用方");
        finish(
            main,
            tool_use_response(
                "toolu_agent",
                "Agent",
                &json!({"description": "找调用方", "prompt": "看看代码",
                        "subagent_type": "Explore", "run_in_background": false}),
            ),
        );
        open_list(&ui).await;
        ui.wait("the main line", |s| {
            list(s)
                .iter()
                .any(|l| l.starts_with("     主会话") && l.ends_with("  当前"))
        })
        .await;
        // 打开时选中子代理（tui-tasks.md §3.2）；enter 查看，卡留着，esc 关上它。
        ui.key(KeyCode::Enter);
        let placeholder = "正在查看 @Explore（只读）· esc 返回";
        ui.wait_for(placeholder).await;
        ui.key(KeyCode::Esc);
        let screen = ui
            .wait("the card to close", |s| {
                list(s).is_empty() && s.contains(placeholder)
            })
            .await;
        let (caption, _) = input_rows(&screen, placeholder);
        let caption_line = screen.lines().nth(usize::from(caption)).unwrap().to_owned();
        assert!(caption_line.trim_end().ends_with("找调用方"), "{screen}");
        finish(child, text_response(&["done"]));
        finish(last, text_response(&["好了"]));
        ui.key(KeyCode::Esc);
        ui.wait_for("好了").await;
    })
    .await;
}
