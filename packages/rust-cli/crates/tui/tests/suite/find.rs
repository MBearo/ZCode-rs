//! 主界面的搜索模式（docs/specs/rust-cli/tui-input.md §2.3，修复线 F114）：真实内核里，空输入框手打 `//` 进入；
//! 折叠的工作段（思考）里的词也搜得到，当前匹配所在的块临时展开；↑ ↓ 换当前匹配，enter 停下并留着展开，esc 回到原处。
use super::support::Shot;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers};
use rust_cli_kernel_api::message::{ContentBlock, StopReason};
use rust_cli_kernel_api::model::{BlockDelta, ModelEvent};
use rust_cli_kernel_api::testing::{FixedClock, text_response, tool_use_response};
use rust_cli_tui::{ColorLevel, Colors};
use rust_cli_tui_kit::render::{Color, attr};
use serde_json::json;

use super::support::{GatedModel, Item, Setup, Ui, finish, with_ui};

/// 一次带思考的回复：思考的文字折进工作段（`Thought for …`），回复照常显示。
fn thinking_reply(thinking: &str, reply: &str) -> Vec<Item> {
    vec![
        Ok(ModelEvent::MessageStart {
            id: "msg".into(),
            model: "scripted".into(),
            usage: Default::default(),
        }),
        Ok(ModelEvent::BlockStart {
            index: 0,
            block: ContentBlock::Thinking {
                thinking: String::new(),
                signature: None,
            },
        }),
        Ok(ModelEvent::BlockDelta {
            index: 0,
            delta: BlockDelta::Thinking(thinking.into()),
        }),
        Ok(ModelEvent::BlockStop { index: 0 }),
        Ok(ModelEvent::BlockStart {
            index: 1,
            block: ContentBlock::Text {
                text: String::new(),
            },
        }),
        Ok(ModelEvent::BlockDelta {
            index: 1,
            delta: BlockDelta::Text(reply.into()),
        }),
        Ok(ModelEvent::BlockStop { index: 1 }),
        Ok(ModelEvent::MessageDelta {
            stop_reason: Some(StopReason::EndTurn),
            usage: Default::default(),
        }),
        Ok(ModelEvent::MessageStop),
    ]
}

fn frame(ui: &Ui) -> Shot {
    ui.frame()
}

/// 画面上第一处 `needle` 的（列，行）：宽字符占两格，续格跳过。
fn cell_of(buffer: &Shot, needle: &str) -> Option<(u16, u16)> {
    let area = buffer.area;
    for y in area.top()..area.bottom() {
        let mut cells: Vec<(u16, &str)> = Vec::new();
        let mut x = area.left();
        while x < area.right() {
            let symbol = buffer[(x, y)].symbol();
            cells.push((x, symbol));
            x += rust_cli_tui_kit::render::width::string_width(symbol).max(1) as u16;
        }
        let row: String = cells.iter().map(|(_, s)| *s).collect();
        if let Some(at) = row.find(needle) {
            let index = row[..at].chars().count();
            return Some((cells[index].0, y));
        }
    }
    None
}

const HINT: &str = "↑↓ previous/next · ⏎ stop here · esc exit";
/// 报告了前景与背景的深色终端：kit 的主题是 rich（tui-kit.md §3.4），当前匹配有底色。
fn rich() -> Setup {
    Setup {
        colors: Colors::new(ColorLevel::TrueColor, Some((0, 0, 0)))
            .with_foreground(Some((255, 255, 255))),
        ..Setup::default()
    }
}

/// 当前匹配的底色：`find_current` 的 `code` 底（tui-kit.md §4.3），期望值经桥求。
fn current() -> Color {
    rich().colors.ink().theme.code
}

#[tokio::test(flavor = "multi_thread")]
async fn typed_slashes_search_the_transcript_including_folded_blocks() {
    let model = GatedModel::scripted(vec![thinking_reply(
        "weighing the options",
        "decided: the options are clear",
    )]);
    // 界面用固定时钟：收起的思考没有用时，一行只有 `Thought`（flow.rs 的同名修复）。
    let setup = Setup {
        ui_clock: Some(Arc::new(FixedClock(0))),
        ..rich()
    };
    with_ui(model, setup, |mut ui| async move {
        let current = current();
        ui.submit("think");
        let screen = ui.wait_for("decided: the options are clear").await;
        assert!(screen.lines().any(|l| l.trim() == "Thought"), "{screen}");
        assert!(!screen.contains("weighing the options"), "{screen}");
        // 手打 `//`：斜杠列表关掉，输入框清空，进入搜索模式。
        ui.key(KeyCode::Char('/'));
        // 命令架的头几行是 Puffin 的顺序（goal、compact、plan…）；`/help` 是别名，不在架上。
        ui.wait_for("/compact").await;
        ui.key(KeyCode::Char('/'));
        let screen = ui.wait_for("⌕  Search the transcript").await;
        assert!(screen.contains(HINT), "{screen}");
        assert!(!screen.contains("/compact"), "{screen}");
        // 边输边搜：思考（折叠的工作段）里一处、回复里一处；先停在离底部最近的那一处。
        ui.type_text("options");
        let screen = ui.wait_for("2/2").await;
        assert!(screen.contains("⌕ options"), "{screen}");
        assert!(!screen.contains("weighing the options"), "{screen}");
        let buffer = frame(&ui);
        let (x, y) = cell_of(&buffer, "options are clear").unwrap();
        assert_eq!(buffer[(x, y)].bg, current);
        assert!(buffer[(x, y)].has(attr::BOLD));
        // ↑：上一个在折叠的工作段里——那一块临时展开，当前匹配换到它。
        ui.key(KeyCode::Up);
        let screen = ui.wait_for("1/2").await;
        assert!(screen.contains("weighing the options"), "{screen}");
        let buffer = frame(&ui);
        let (x, y) = cell_of(&buffer, "options are clear").unwrap();
        assert_ne!(buffer[(x, y)].bg, current);
        assert!(buffer[(x, y)].has(attr::REVERSE));
        let (x, y) = cell_of(&buffer, "weighing the options").unwrap();
        assert_eq!(buffer[(x + 13, y)].bg, current);
        // ctrl+n 回到下一个，回绕；ctrl+p 回来。
        ui.press(KeyCode::Char('n'), KeyModifiers::CONTROL);
        ui.wait_for("2/2").await;
        ui.press(KeyCode::Down, KeyModifiers::NONE);
        ui.wait_for("1/2").await;
        // esc：回到原处，临时展开的块收起，footer 恢复。
        ui.key(KeyCode::Esc);
        let screen = ui.wait("search mode to end", |s| !s.contains(HINT)).await;
        assert!(!screen.contains("weighing the options"), "{screen}");
        assert!(screen.contains("? keys"), "{screen}");
        // 再进一次，enter 停在折叠块里的匹配：那一块留着展开。
        ui.type_text("//weigh");
        ui.wait_for("1/1").await;
        ui.key(KeyCode::Enter);
        let screen = ui.wait("search mode to end", |s| !s.contains(HINT)).await;
        assert!(screen.contains("weighing the options"), "{screen}");
        assert!(!screen.contains("⌕"), "{screen}");
        // 没有匹配：写 `no matches`，enter 同 esc。
        ui.type_text("//zzz");
        ui.wait_for("no matches").await;
        ui.key(KeyCode::Backspace);
        ui.key(KeyCode::Backspace);
        ui.key(KeyCode::Backspace);
        ui.wait_for("⌕  Search the transcript").await;
        // 查询空了再按 backspace：退出。
        ui.key(KeyCode::Backspace);
        ui.wait("search mode to end", |s| !s.contains(HINT)).await;
        // 粘贴进来的 `//` 不切模式，留在输入框里。
        ui.paste("//");
        let screen = ui.wait_for("› //").await;
        assert!(!screen.contains(HINT), "{screen}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn chinese_shows_the_search_box_and_hint() {
    let model = GatedModel::scripted(vec![text_response(&["第一行写着搜索的词"])]);
    let setup = Setup {
        lang: "zh_CN.UTF-8",
        ..rich()
    };
    with_ui(model, setup, |mut ui| async move {
        let current = current();
        ui.submit("说点什么");
        ui.wait_for("第一行写着搜索的词").await;
        ui.type_text("//");
        let screen = ui.wait_for("⌕  搜索对话记录").await;
        assert!(
            screen.contains("↑↓ 上一个/下一个 · ⏎ 停在这里 · esc 退出"),
            "{screen}"
        );
        ui.type_text("搜索");
        let screen = ui.wait_for("1/1").await;
        let buffer = frame(&ui);
        let (x, y) = cell_of(&buffer, "搜索的词").unwrap();
        // 宽字符：当前匹配盖住两个字（每个字的第一格带样式，第二格是宽字符的续格）。
        for dx in [0, 2] {
            assert_eq!(buffer[(x + dx, y)].bg, current, "{screen}");
        }
        assert_ne!(buffer[(x + 4, y)].bg, current, "{screen}");
        ui.type_text("不存在");
        ui.wait_for("没有匹配").await;
        ui.key(KeyCode::Esc);
        ui.wait("search mode to end", |s| !s.contains("停在这里"))
            .await;
    })
    .await;
}

/// 查看子代理时（tui-panels.md §1.6）：`/` 进入搜索模式，搜的是被查看的子会话；esc 先退出搜索，再按才返回主会话。
#[tokio::test(flavor = "multi_thread")]
async fn viewing_a_subagent_searches_its_transcript() {
    let (model, senders) = GatedModel::new(3);
    let mut senders = senders.into_iter();
    let (main, child, last) = (
        senders.next().unwrap(),
        senders.next().unwrap(),
        senders.next().unwrap(),
    );
    let setup = Setup {
        height: 32,
        fork_subagent: Some(false),
        ..Setup::default()
    };
    with_ui(model, setup, |mut ui| async move {
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
        // ↓ 打开子代理的层（照 Puffin 的 picker，tui-kit.md §4.3），选中子代理，enter 查看。
        super::agents_tree::open_list(&ui).await;
        ui.wait("the agent list", |s| {
            super::agents_tree::list(s)
                .iter()
                .any(|l| l.starts_with("   ○ Explore  Find callers"))
        })
        .await;
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Enter);
        ui.wait_for("↳ Explore · Find callers").await;
        // 手打 `//`：第一个 `/` 进入，紧跟的第二个丢掉。
        ui.type_text("//");
        let screen = ui.wait_for("⌕  Search the transcript").await;
        assert!(screen.contains(HINT), "{screen}");
        // 主会话的输入 `find the callers` 不在子会话里；子会话的 prompt 在。
        ui.type_text("find the");
        ui.wait_for("no matches").await;
        for _ in 0.."find the".len() {
            ui.key(KeyCode::Backspace);
        }
        ui.type_text("around");
        ui.wait_for("1/1").await;
        ui.key(KeyCode::Esc);
        let screen = ui.wait("search mode to end", |s| !s.contains(HINT)).await;
        assert!(screen.contains("↳ Explore"), "{screen}");
        finish(child, text_response(&["done"]));
        finish(last, text_response(&["callers found"]));
        ui.key(KeyCode::Esc);
        ui.wait_for("callers found").await;
    })
    .await;
}
