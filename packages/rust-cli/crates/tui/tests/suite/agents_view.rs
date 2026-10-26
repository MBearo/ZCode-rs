//! 输入框下面的子代理列表与只读查看（docs/specs/rust-cli/tui-panels.md §1）：内核是 LocalKernel（agents 的
//! Agent 工具），列表来自 `subagents()`，查看的内容来自子会话的存储。
use crossterm::event::KeyCode;
use ratatui::buffer::Buffer;
use ratatui::style::Color;
use rust_cli_kernel_api::testing::{text_response, tool_use_response};
use serde_json::json;

use super::support::{GatedModel, Setup, Ui, finish, row_of, send, with_ui};

/// 修复线 F62（tui-panels.md §1.6）：查看子代理时输入框的边线与标签的颜色。
const CYAN: Color = Color::Rgb(8, 145, 178);

fn frame(ui: &Ui) -> Buffer {
    ui.buffer.lock().unwrap().clone().unwrap()
}

/// 输入框的上边线、`❯ ` 那一行与下边线的行号（`placeholder` 是占位文字）。
fn input_rows(screen: &str, placeholder: &str) -> (u16, u16) {
    let row = row_of(screen, &format!("❯ {placeholder}")).unwrap();
    (row - 1, row + 1)
}

fn tall() -> Setup {
    Setup {
        height: 32,
        // 前台子代理只在 fork 关着时有（`ZCODE_FORK_SUBAGENT=0`，修复线 L-A，agents.md §8.1.1）。
        fork_subagent: Some(false),
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
        // footer 下面：`main` 与子代理一行（类型、描述、用时）。
        let screen = ui
            .wait("the agent list", |s| {
                s.lines().any(|l| l.contains("◯ Explore  Find callers"))
            })
            .await;
        assert!(screen.lines().any(|l| l.ends_with(" main")), "{screen}");
        // 输入框为空时 ↓ 进入列表；再按 ↓ 选中子代理，enter 查看。
        ui.key(KeyCode::Down);
        ui.wait_for("↑/↓ to select · enter to view · esc to close")
            .await;
        ui.key(KeyCode::Down);
        ui.wait_for("› ◯ Explore").await;
        ui.key(KeyCode::Enter);
        let screen = ui.wait_for("↳ Explore · Find callers").await;
        assert!(screen.contains("Look around the code"), "{screen}");
        // 修复线 F62：占位文字写 `@类型` 并写明只读；上下边线是子代理色，上边线右侧
        // 用同色底色、黑字（深色背景）标出描述，代替会话标题。
        let placeholder = "Viewing @Explore (read-only) · esc to return";
        assert!(screen.contains(placeholder), "{screen}");
        let (top, bottom) = input_rows(&screen, placeholder);
        let buffer = frame(&ui);
        let width = buffer.area.width;
        let top_line: String = (0..width).map(|x| buffer[(x, top)].symbol()).collect();
        assert!(top_line.ends_with(" Find callers ─"), "{top_line}");
        assert_eq!(buffer[(0, top)].fg, CYAN);
        assert_eq!(buffer[(width - 1, top)].fg, CYAN);
        let label = &buffer[(width - 3, top)];
        assert_eq!((label.bg, label.fg), (CYAN, Color::Rgb(0, 0, 0)));
        assert!((0..width).all(|x| buffer[(x, bottom)].fg == CYAN));
        assert!(!screen.contains("Explore(Find callers)"), "{screen}");
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
        assert!(screen.contains("Explore(Find callers)"), "{screen}");
        assert!(!screen.contains("↳ Explore"), "{screen}");
        assert!(!screen.contains("Viewing @Explore"), "{screen}");
        let buffer = frame(&ui);
        // 输入框是画面上最后一个以 `❯` 开头的行（上面 transcript 里的用户消息也以 `❯` 开头）。
        let row = (0..buffer.area.height)
            .rev()
            .find(|y| buffer[(0, *y)].symbol() == "❯")
            .unwrap();
        for y in [row - 1, row + 1] {
            assert!((0..width).all(|x| buffer[(x, y)].fg != CYAN && buffer[(x, y)].bg != CYAN));
        }
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
        ui.wait("the main line", |s| {
            s.lines().any(|l| l.ends_with(" 主会话"))
        })
        .await;
        ui.key(KeyCode::Down);
        ui.wait_for("↑/↓ 选择 · enter 查看 · esc 关闭").await;
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Enter);
        let placeholder = "正在查看 @Explore（只读）· esc 返回";
        let screen = ui.wait_for(placeholder).await;
        let (top, _) = input_rows(&screen, placeholder);
        let top_line: String = screen.lines().nth(usize::from(top)).unwrap().to_owned();
        assert!(top_line.ends_with(" 找调用方 ─"), "{screen}");
        finish(child, text_response(&["done"]));
        finish(last, text_response(&["好了"]));
        ui.key(KeyCode::Esc);
        ui.wait_for("好了").await;
    })
    .await;
}
