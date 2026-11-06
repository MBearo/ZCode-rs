//! 流式回复的排版（docs/specs/rust-cli/tui.md §4.1，修复线 F6，审查 R5-5）：同一段回复分很多个增量与一次给全文，
//! transcript 的画面（含样式，逐格）相同；回复结束后的画面也相同。增量很多时界面一次唤醒处理完就绪的事件再画。
use std::sync::{Arc, Mutex};

use super::support::Shot;
use rust_cli_kernel_api::model::{BlockDelta, ModelEvent};
use rust_cli_kernel_api::testing::{text_response, tool_use_response};
use serde_json::json;

use super::support::{GatedModel, Setup, finish, send, with_ui};

/// 约 6 KB 的 markdown：标题、强调、行内代码、链接、代码块、列表；最后一行是标记。
fn body() -> String {
    let mut text = String::new();
    for i in 0..12 {
        text.push_str(&format!(
            "## Section {i}\n\nSome **bold** text and `code` with a [link](https://example.com/{i}). Lorem ipsum dolor sit amet, consectetur adipiscing elit.\n\n"
        ));
        text.push_str("```rust\nfn main() {\n    let x = vec![1, 2, 3];\n}\n```\n\n");
        text.push_str(&format!("- item one {i}\n- item two {i}\n\n"));
    }
    text.push_str("END-MARK\n");
    text
}

/// transcript 部分（状态行以上）每行的文字与格子（含样式）。
fn transcript(buffer: &Shot) -> Vec<(String, String)> {
    let area = buffer.area;
    let mut rows = Vec::new();
    for y in area.top()..area.bottom() {
        let row: Vec<_> = (area.left()..area.right())
            .map(|x| buffer[(x, y)].clone())
            .collect();
        let text: String = row.iter().map(super::support::ShotCell::symbol).collect();
        // 工作行（`   ⋮⋮⋮ Polishing…  0s  ·  ↓ 667 tokens`，tui-kit.md §4.2「工作行」）的光标、动词与用时按真实时间变，不比。
        if text.starts_with('─') || text.contains("esc stop") || text.contains(" tokens") {
            break;
        }
        rows.push((text, format!("{row:?}")));
    }
    rows
}

/// 流式中会动的末尾几行：笔尖在呼吸、最新的字从浅处落定（Puffin 的笔，tui-kit.md §4.2「回答」），样式按真实时间变。
const MOVING_ROWS: usize = 3;

/// 流式中的画面里稳定的部分：每行的文字；末尾 [`MOVING_ROWS`] 个非空行之外的行的格子（含样式）。
fn steady(rows: &[(String, String)]) -> Vec<(String, String)> {
    let last = rows
        .iter()
        .rposition(|(text, _)| !text.trim().is_empty())
        .unwrap_or(0);
    let moving: Vec<usize> = (0..=last)
        .rev()
        .filter(|&i| !rows[i].0.trim().is_empty())
        .take(MOVING_ROWS)
        .collect();
    rows.iter()
        .enumerate()
        .map(|(i, (text, cells))| {
            let cells = if moving.contains(&i) {
                String::new()
            } else {
                cells.clone()
            };
            (text.clone(), cells)
        })
        .collect()
}

/// 把回复按 `size` 字节一片流出：流式中（标记已显示）与结束后的画面。
async fn screens(size: usize) -> (Vec<(String, String)>, Vec<(String, String)>) {
    let text = body();
    let chunks: Vec<String> = text
        .as_bytes()
        .chunks(size)
        .map(|chunk| String::from_utf8(chunk.to_vec()).unwrap())
        .collect();
    let refs: Vec<&str> = chunks.iter().map(String::as_str).collect();
    let mut events = text_response(&refs);
    let tail = events.split_off(events.len() - 3);
    let (model, mut senders) = GatedModel::new(1);
    let sender = senders.remove(0);
    let shots = Arc::new(Mutex::new(Vec::new()));
    let taken = shots.clone();
    let setup = Setup {
        width: 100,
        height: 50,
        ..Setup::default()
    };
    with_ui(model, setup, |mut ui| async move {
        ui.submit("go");
        send(&sender, events);
        ui.wait_for("END-MARK").await;
        let shot = |ui: &super::support::Ui| ui.frame();
        taken.lock().unwrap().push(transcript(&shot(&ui)));
        finish(sender, tail);
        ui.wait("the turn to end", |s| !s.contains("esc stop"))
            .await;
        taken.lock().unwrap().push(transcript(&shot(&ui)));
    })
    .await;
    let mut shots = shots.lock().unwrap().split_off(0);
    let done = shots.pop().unwrap();
    (shots.pop().unwrap(), done)
}

#[tokio::test]
async fn many_small_deltas_draw_the_same_screen_as_one_delta() {
    let (whole_streaming, whole_done) = screens(usize::MAX).await;
    assert!(
        whole_streaming
            .iter()
            .any(|(text, _)| text.contains("Section 11"))
    );
    for size in [7, 64] {
        let (streaming, done) = screens(size).await;
        assert_eq!(
            steady(&streaming),
            steady(&whole_streaming),
            "chunks of {size} bytes, streaming"
        );
        assert_eq!(done, whole_done, "chunks of {size} bytes, done");
    }
}

/// 回答说完一句、模型接着写下一个调用的输入（调用要等输入写完才有行）：回答末尾的笔尖换成点阵，下一个词落下时它离开
/// （docs/puffin-tui.md §4.2「手停住」）。真实内核，前一个调用走完了。修前这里一直只有笔尖。
#[tokio::test(flavor = "multi_thread")]
async fn the_lattice_ends_a_stopped_answer_between_tool_calls() {
    let (model, mut senders) = GatedModel::new(2);
    let answer = senders.pop().unwrap();
    finish(
        senders.pop().unwrap(),
        tool_use_response(
            "t1",
            "TaskCreate",
            &json!({"subject": "Check it", "description": "Check it"}),
        ),
    );
    let mut events = text_response(&["Now the next thing."]);
    let after = events.split_off(events.len() - 3);
    send(&answer, events);
    with_ui(model, Setup::default(), |mut ui| async move {
        ui.submit("go");
        ui.wait_for("thing. ⋮⋮⋮").await;
        send(
            &answer,
            vec![Ok(ModelEvent::BlockDelta {
                index: 0,
                delta: BlockDelta::Text(" More words.".to_owned()),
            })],
        );
        let screen = ui.wait_for("More words").await;
        assert!(!screen.contains("thing. ⋮⋮⋮"), "{screen}");
        finish(answer, after);
        ui.wait("the turn ends", |s| !s.contains("esc stop")).await;
    })
    .await;
}
