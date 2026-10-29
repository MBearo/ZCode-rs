//! 查看子代理时 footer 跟着切（docs/specs/rust-cli/tui-panels.md §1.7，修复线 F112，用户要求 2026-10-29）：真实的内核组装
//! （会话 actor、agents、压缩模块与用量表的查询）。前台 Explore 的请求用量与计时都与主会话不同；查看它时 footer 写它的
//! TPS、ctx 与 cache，左侧不写 `esc to interrupt`；esc 回到主会话后 footer 与查看前相同。替身模型不在模型卡片里：
//! 有效窗口 180,000（同 footer_stats.rs）。
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crossterm::event::KeyCode;
use rust_cli_kernel_api::ids::ToolUseId;
use rust_cli_kernel_api::message::{ContentBlock, StopReason, Usage};
use rust_cli_kernel_api::model::{BlockDelta, ModelEvent};
use rust_cli_kernel_api::testing::text_response;
use serde_json::json;

use super::support::{GatedModel, Item, Setup, finish, send, with_ui};

/// 测试拨动的内核时钟（请求的开始、首个内容与结束各读一次）。
#[derive(Default)]
struct ManualClock(AtomicU64);

impl ManualClock {
    fn set(&self, ms: u64) {
        self.0.store(ms, Ordering::SeqCst);
    }
}

impl rust_cli_kernel_api::clock::Clock for ManualClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }

    fn time_zone(&self) -> String {
        "UTC".to_owned()
    }
}

fn usage(input: u64, read: u64, output: u64) -> Usage {
    Usage {
        input_tokens: input,
        output_tokens: output,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: read,
    }
}

fn start(input: Usage) -> Item {
    Ok(ModelEvent::MessageStart {
        id: "msg_api".to_owned(),
        model: "scripted".to_owned(),
        usage: input,
    })
}

fn end(stop: StopReason, input: Usage, output: u64) -> Vec<Item> {
    vec![
        Ok(ModelEvent::BlockStop { index: 0 }),
        Ok(ModelEvent::MessageDelta {
            stop_reason: Some(stop),
            usage: Usage {
                output_tokens: output,
                ..input
            },
        }),
        Ok(ModelEvent::MessageStop),
    ]
}

/// 主会话的第一个请求：派生前台 Explore（输入 200、缓存读 800 → 主会话的 cache 80.0%，ctx 0.6%）。
fn spawn_agent() -> Vec<Item> {
    let input = usage(200, 800, 0);
    let mut events = vec![
        start(input),
        Ok(ModelEvent::BlockStart {
            index: 0,
            block: ContentBlock::ToolUse {
                id: ToolUseId::new("toolu_agent"),
                name: "Agent".to_owned(),
                input: serde_json::Value::Object(serde_json::Map::new()),
            },
        }),
        Ok(ModelEvent::BlockDelta {
            index: 0,
            delta: BlockDelta::InputJson(
                json!({"description": "Find callers", "prompt": "Look around the code",
                       "subagent_type": "Explore", "run_in_background": false})
                .to_string(),
            ),
        }),
    ];
    events.extend(end(StopReason::ToolUse, input, 120));
    events
}

/// 子代理回复的开头（第一个文字增量）。
fn child_head(input: Usage) -> Vec<Item> {
    vec![
        start(input),
        Ok(ModelEvent::BlockStart {
            index: 0,
            block: ContentBlock::Text {
                text: String::new(),
            },
        }),
        Ok(ModelEvent::BlockDelta {
            index: 0,
            delta: BlockDelta::Text("Checking the callers\n".to_owned()),
        }),
    ]
}

/// 等模型收到第 `count` 个请求。
async fn requested(model: &GatedModel, count: usize) {
    let wait = async {
        while model.requests().len() < count {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };
    if tokio::time::timeout(Duration::from_secs(10), wait)
        .await
        .is_err()
    {
        panic!("timed out waiting for model request {count}");
    }
}

/// footer 那一行（有 ctx 的那一行；transcript 顶上的启动头也写模型名，不能按模型名找）。
fn footer_line(screen: &str) -> String {
    screen
        .lines()
        .find(|line| line.contains(" · ctx "))
        .unwrap_or_default()
        .to_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn viewing_a_subagent_switches_the_footer_stats() {
    let (model, senders) = GatedModel::new(3);
    let mut senders = senders.into_iter();
    let (main, child, last) = (
        senders.next().unwrap(),
        senders.next().unwrap(),
        senders.next().unwrap(),
    );
    let clock = Arc::new(ManualClock::default());
    let setup = Setup {
        width: 120,
        height: 32,
        clock: Some(clock.clone()),
        // 前台子代理只在 fork 关着时有（agents.md §8.1.1）。
        fork_subagent: Some(false),
        ..Setup::default()
    };
    let gated = model.clone();
    with_ui(model, setup, |mut ui| async move {
        clock.set(1_000);
        ui.submit("find the callers");
        requested(&gated, 1).await;
        clock.set(6_000);
        finish(main, spawn_agent());
        // 子代理的请求在 6000 开始；第一个文字在 7000 到达，8000 结束，输出 90 → 90 tok/s。
        requested(&gated, 2).await;
        clock.set(7_000);
        let child_input = usage(1_000, 3_000, 0);
        send(&child, child_head(child_input));
        ui.wait_for("Checking the callers").await;
        clock.set(8_000);
        finish(child, end(StopReason::EndTurn, child_input, 90));
        // 子代理结束后主会话的下一个请求开始（不放行，主会话一直在跑）。
        requested(&gated, 3).await;
        let main_screen = ui
            .wait("the main stats", |s| footer_line(s).contains("cache 80.0%"))
            .await;
        let main_footer = footer_line(&main_screen);
        assert!(main_footer.contains("ctx 0.6%"), "{main_footer}");
        assert!(!main_footer.contains("90 tok/s"), "{main_footer}");
        assert!(main_footer.contains("esc to interrupt"), "{main_footer}");
        // ↓ 进入列表，再 ↓ 选中子代理，enter 查看。
        ui.key(KeyCode::Down);
        ui.wait_for("↑/↓ to select · enter to view · esc to close")
            .await;
        ui.key(KeyCode::Down);
        ui.wait_for("› ◯ Explore").await;
        ui.key(KeyCode::Enter);
        // 子代理：输入 1000 + 缓存读 3000 → cache 75.0%；上下文 4090 / 180,000 → ctx 2.3%。
        let screen = ui
            .wait("the subagent stats", |s| {
                s.contains("↳ Explore") && footer_line(s).contains("cache 75.0%")
            })
            .await;
        let viewed = footer_line(&screen);
        assert!(
            viewed.contains("scripted · 90 tok/s · ctx 2.3% · cache 75.0%"),
            "{viewed}"
        );
        for main in ["cache 80.0%", "ctx 0.6%", "esc to interrupt"] {
            assert!(!viewed.contains(main), "{main}: {viewed}");
        }
        // esc 回到主会话：footer 与查看前相同。
        ui.key(KeyCode::Esc);
        let back = ui
            .wait("the main stats again", |s| {
                !s.contains("↳ Explore") && footer_line(s).contains("cache 80.0%")
            })
            .await;
        assert_eq!(footer_line(&back), main_footer);
        finish(last, text_response(&["callers found"]));
        ui.wait_for("callers found").await;
    })
    .await;
}
