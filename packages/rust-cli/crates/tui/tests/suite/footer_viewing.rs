//! 查看子代理时 footer 跟着切（docs/specs/rust-cli/tui-panels.md §1.7，修复线 F112，用户要求 2026-10-29）：真实的内核组装
//! （会话 actor、agents、压缩模块与用量表的查询）。前台 Explore 的请求用量与计时都与主会话不同；查看它时 footer 写它的
//! TPS、ctx 与 cache，槽里不写 `esc stop`；esc 回到主会话后 footer 与查看前相同。替身模型不在模型卡片里：
//! 有效窗口 180,000（同 footer_stats.rs）。
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::KeyCode;
use rust_cli_kernel_api::ids::ToolUseId;
use rust_cli_kernel_api::message::{ContentBlock, StopReason, Usage};
use rust_cli_kernel_api::model::{BlockDelta, ModelEvent};
use rust_cli_kernel_api::testing::text_response;
use serde_json::json;

use super::support::{GatedModel, Item, ManualClock, Setup, finish, send, with_ui};

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

/// 主会话的第一个请求：派生前台 Explore（输入 200、缓存读 800 → 主会话的命中率 80.0%，ctx 0.6%）。
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

/// 状态栏有上下文的那一行（`用掉的/180k`；transcript 顶上的启动头也写模型名，不能按模型名找；S2 起照 Puffin，
/// tui-kit.md §4.2「状态栏」）。`esc stop` 在输入行的槽里，不在这一行。
fn footer_line(screen: &str) -> String {
    screen
        .lines()
        .find(|line| line.contains("/180k"))
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
        // 主 transcript 里子代理是一行调用（同 Puffin `tool_row::agent_row`），不写它说的话，画面上看不出这一批事件到了：
        // 等内核处理完它们（第 2 个响应的前 3 个事件），时钟才走到 8000。以前等 Agent 行下面的 `Checking the callers`。
        gated.consumed(1, 3).await;
        clock.set(8_000);
        finish(child, end(StopReason::EndTurn, child_input, 90));
        // 子代理结束后主会话的下一个请求开始（不放行，主会话一直在跑）。
        requested(&gated, 3).await;
        let main_screen = ui
            .wait("the main stats", |s| {
                footer_line(s).contains(" 1%  1.1k/180k")
            })
            .await;
        let main_footer = footer_line(&main_screen);
        assert!(!main_footer.contains("cache"), "{main_footer}");
        assert!(!main_footer.contains("90 tok/s"), "{main_footer}");
        assert!(main_screen.contains("esc stop"), "{main_screen}");
        // ↓ 打开任务卡，选中子代理（tui-tasks.md §3.2），enter 查看。
        super::agents_tree::open_list(&ui).await;
        // 选中只改墨（名字 brand，没有指针，tui-kit.md §4.3）：等层里 `Explore` 的名字（第 5 列起，记号在第 3 列）变成 brand。
        let brand = Setup::default().colors.ink().theme.brand;
        let selected = async {
            loop {
                let shot = ui.frame();
                let row = (0..shot.area.height).rev().find(|y| {
                    let row = shot.row(*y);
                    row.starts_with("   ")
                        && row
                            .chars()
                            .skip(5)
                            .collect::<String>()
                            .starts_with("Explore")
                });
                if row.is_some_and(|y| shot[(5, y)].fg == brand) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(10), selected)
            .await
            .unwrap_or_else(|_| panic!("the selection on Explore:\n{}", ui.screen()));
        ui.key(KeyCode::Enter);
        // 子代理：输入 1000 + 缓存读 3000 → 命中率 75.0%（只在 `/stats` 的卡里）；上下文 4090 / 180,000 → 2%（Puffin 的整数）。
        let screen = ui
            .wait("the subagent stats", |s| {
                s.contains("↳ Explore") && footer_line(s).contains(" 2%  4.1k/180k  ·  90 tok/s")
            })
            .await;
        let viewed = footer_line(&screen);
        for main in ["cache", "1.1k/180k"] {
            assert!(!viewed.contains(main), "{main}: {viewed}");
        }
        assert!(!screen.contains("esc stop"), "{screen}");
        // 卡留着（tui-tasks.md §3.3）：第一个 esc 关上它；第二个回到主会话，footer 与查看前相同。
        ui.key(KeyCode::Esc);
        ui.wait("the card to close", |s| {
            super::agents_tree::list(s).is_empty()
        })
        .await;
        ui.key(KeyCode::Esc);
        let back = ui
            .wait("the main stats again", |s| {
                !s.contains("↳ Explore") && footer_line(s).contains(" 1%  1.1k/180k")
            })
            .await;
        assert_eq!(footer_line(&back), main_footer);
        finish(last, text_response(&["callers found"]));
        ui.wait_for("callers found").await;
    })
    .await;
}

/// 任务卡的 Subagents 的行有三列数字（tui-panels.md §1.3，2026-11-02 补齐；以前只有用时）：数字是 runner 经
/// `request_stats(子会话)` 查来的。子代理的一个请求：输入 1000 + 缓存读 3000 + 输出 90 → `4.1k tok`；生成 1000 ms、输出
/// 90 → 平均 `90 tok/s`。结束的行的 token 是 Agent 调用报告的（这里同是 4090）。
#[tokio::test(flavor = "multi_thread")]
async fn the_card_rows_carry_the_subagent_figures() {
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
        requested(&gated, 2).await;
        clock.set(7_000);
        let child_input = usage(1_000, 3_000, 0);
        send(&child, child_head(child_input));
        gated.consumed(1, 3).await;
        clock.set(8_000);
        finish(child, end(StopReason::EndTurn, child_input, 90));
        requested(&gated, 3).await;
        super::agents_tree::open_list(&ui).await;
        let screen = ui
            .wait("the figures on the row", |s| {
                s.lines().any(|l| {
                    l.starts_with("   ✓ Explore") && l.trim_end().ends_with("4.1k tok   90 tok/s")
                })
            })
            .await;
        // 三列在量度的右沿（宽 − 2，至多 99）。
        let row = screen
            .lines()
            .find(|l| l.starts_with("   ✓ Explore"))
            .unwrap();
        assert_eq!(row.trim_end().chars().count(), 99, "{screen}");
        ui.key(KeyCode::Esc);
        finish(last, text_response(&["callers found"]));
        ui.wait_for("callers found").await;
    })
    .await;
}
