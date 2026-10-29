//! 卡住时的可见性（docs/specs/rust-cli/stall-visibility.md §3、§4）：主线程状态行的重试与停顿行，前台子代理
//! Agent 行的推理项与重试 / 停顿行，输入框下面子代理列表的当前活动（前台与后台）。内核是 LocalKernel；模型替身
//! 按请求给出带通知的流，测试决定什么时候发出。
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use futures_util::stream::{self, BoxStream, StreamExt};
use rust_cli_kernel_api::model::{
    ModelError, ModelEvent, ModelNotice, ModelPort, ModelRequest, ModelStreamItem,
};
use rust_cli_kernel_api::testing::{text_response, tool_use_response};
use serde_json::json;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::support::{Setup, with_ui};

type Item = Result<ModelStreamItem, ModelError>;

/// 每次请求取下一个通道；通道里是事件与通知。
struct NoticeModel {
    responses: Mutex<VecDeque<mpsc::UnboundedReceiver<Item>>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl NoticeModel {
    fn new(count: usize) -> (Arc<Self>, Vec<mpsc::UnboundedSender<Item>>) {
        let (senders, receivers): (Vec<_>, VecDeque<_>) =
            (0..count).map(|_| mpsc::unbounded_channel()).unzip();
        let model = Arc::new(Self {
            responses: Mutex::new(receivers),
            requests: Mutex::default(),
        });
        (model, senders)
    }
}

impl ModelPort for NoticeModel {
    fn stream(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<ModelEvent, ModelError>> {
        self.stream_with_notices(request, cancel)
            .filter_map(|item| async move {
                match item {
                    Ok(ModelStreamItem::Event(event)) => Some(Ok(event)),
                    Ok(ModelStreamItem::Notice(_)) => None,
                    Err(error) => Some(Err(error)),
                }
            })
            .boxed()
    }

    fn stream_with_notices(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Item> {
        self.requests.lock().unwrap().push(request);
        let Some(receiver) = self.responses.lock().unwrap().pop_front() else {
            return stream::iter([Err(ModelError::Protocol {
                message: "no scripted response left".to_owned(),
            })])
            .boxed();
        };
        stream::unfold(receiver, |mut receiver| async move {
            receiver.recv().await.map(|item| (item, receiver))
        })
        .take_until(cancel.cancelled_owned())
        .boxed()
    }
}

fn events(sender: &mpsc::UnboundedSender<Item>, events: Vec<Result<ModelEvent, ModelError>>) {
    for event in events {
        sender.send(event.map(ModelStreamItem::Event)).unwrap();
    }
}

fn notice(sender: &mpsc::UnboundedSender<Item>, notice: ModelNotice) {
    sender.send(Ok(ModelStreamItem::Notice(notice))).unwrap();
}

fn retrying(delay_ms: u64) -> ModelNotice {
    ModelNotice::Retrying {
        attempt: 1,
        max_retries: 10,
        delay_ms,
        error: ModelError::Overloaded {
            message: "Overloaded".to_owned(),
        },
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_status_line_shows_a_retry_until_the_response_arrives() {
    let (model, senders) = NoticeModel::new(1);
    let main = senders.into_iter().next().unwrap();
    with_ui(model, Setup::default(), |mut ui| async move {
        ui.submit("hello");
        notice(&main, retrying(60_000));
        let screen = ui.wait_for("✻ API error · Retrying in").await;
        assert!(screen.contains("attempt 1/10"), "{screen}");
        // 重发拿到响应头：回到 spinner。
        notice(&main, ModelNotice::Resumed);
        ui.wait("the spinner", |screen| {
            !screen.contains("Retrying in") && screen.contains("esc to interrupt")
        })
        .await;
        // 停顿：等待 API 响应与看门狗的倒计时；之后的流增量清掉它。
        notice(
            &main,
            ModelNotice::Stalled {
                retry_in_ms: 280_000,
            },
        );
        ui.wait_for("✻ Waiting for API response · will retry in 4m")
            .await;
        events(&main, text_response(&["hi there"]));
        drop(main);
        let screen = ui.wait_for("hi there").await;
        assert!(!screen.contains("Waiting for API response"), "{screen}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_foreground_agent_shows_its_retry_under_the_agent_row() {
    let (model, senders) = NoticeModel::new(3);
    let mut senders = senders.into_iter();
    let (main, child, last) = (
        senders.next().unwrap(),
        senders.next().unwrap(),
        senders.next().unwrap(),
    );
    let setup = Setup {
        width: 100,
        // 前台子代理只在 fork 关着时有（`ZCODE_FORK_SUBAGENT=0`，修复线 L-A，agents.md §8.1.1）。
        fork_subagent: Some(false),
        ..Setup::default()
    };
    with_ui(model, setup, |mut ui| async move {
        ui.submit("find the callers");
        events(
            &main,
            tool_use_response(
                "toolu_agent",
                "Agent",
                &json!({"description": "Find callers", "prompt": "Look around",
                        "subagent_type": "Explore", "run_in_background": false}),
            ),
        );
        drop(main);
        ui.wait_for("Initializing…").await;
        notice(
            &child,
            ModelNotice::Stalled {
                retry_in_ms: 100_000,
            },
        );
        let screen = ui
            .wait_for("✻ Waiting for API response · will retry in 1m 40s")
            .await;
        assert!(screen.contains("Explore(Find callers)"), "{screen}");
        notice(&child, retrying(30_000));
        ui.wait_for("✻ API error · Retrying in").await;
        notice(&child, ModelNotice::Resumed);
        ui.wait("the retry line to go", |screen| {
            !screen.contains("Retrying in")
        })
        .await;
        events(&child, text_response(&["Checked"]));
        drop(child);
        events(&last, text_response(&["callers found"]));
        drop(last);
        ui.wait_for("callers found").await;
    })
    .await;
}

/// 推理块的开头与一段推理增量。
/// 接在 [`thinking`] 之后：结束 thinking 块，再给一段文字并结束这次回复。
fn finish_with_text(sender: &mpsc::UnboundedSender<Item>, text: &str) {
    use rust_cli_kernel_api::message::{ContentBlock, StopReason, Usage};
    use rust_cli_kernel_api::model::BlockDelta;
    events(
        sender,
        vec![
            Ok(ModelEvent::BlockStop { index: 0 }),
            Ok(ModelEvent::BlockStart {
                index: 1,
                block: ContentBlock::Text {
                    text: String::new(),
                },
            }),
            Ok(ModelEvent::BlockDelta {
                index: 1,
                delta: BlockDelta::Text(text.to_owned()),
            }),
            Ok(ModelEvent::BlockStop { index: 1 }),
            Ok(ModelEvent::MessageDelta {
                stop_reason: Some(StopReason::EndTurn),
                usage: Usage::default(),
            }),
            Ok(ModelEvent::MessageStop),
        ],
    );
}

fn thinking(sender: &mpsc::UnboundedSender<Item>, text: &str) {
    use rust_cli_kernel_api::message::ContentBlock;
    use rust_cli_kernel_api::model::BlockDelta;
    events(
        sender,
        vec![
            Ok(ModelEvent::MessageStart {
                id: "msg_child".to_owned(),
                model: "scripted".to_owned(),
                usage: rust_cli_kernel_api::message::Usage::default(),
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
                delta: BlockDelta::Thinking(text.to_owned()),
            }),
        ],
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_foreground_agent_shows_its_thinking_in_the_row_only() {
    let (model, senders) = NoticeModel::new(3);
    let mut senders = senders.into_iter();
    let (main, child, last) = (
        senders.next().unwrap(),
        senders.next().unwrap(),
        senders.next().unwrap(),
    );
    let setup = Setup {
        width: 100,
        height: 30,
        // 前台子代理只在 fork 关着时有（`ZCODE_FORK_SUBAGENT=0`，修复线 L-A，agents.md §8.1.1）。
        fork_subagent: Some(false),
        ..Setup::default()
    };
    with_ui(model, setup, |mut ui| async move {
        ui.submit("find the callers");
        events(
            &main,
            tool_use_response(
                "toolu_agent",
                "Agent",
                &json!({"description": "Find callers", "prompt": "Look around",
                        "subagent_type": "Explore", "run_in_background": false}),
            ),
        );
        drop(main);
        ui.wait_for("Initializing…").await;
        thinking(&child, "Let me   look\naround");
        // Agent 行：推理项；列表行没有“思考中”（修复线 F26，用户决定）。列表行来自另一份快照（子代理列表的查询），
        // 也等它画出来再看（修复线 F32：原来只等 Agent 行，列表行还没到时下面的断言空过）。
        let screen = ui
            .wait("thinking in the row", |screen| {
                screen.contains("⎿  ∴ Let me look around")
                    && screen
                        .lines()
                        .any(|l| l.contains('◯') && l.contains("Find callers"))
            })
            .await;
        assert!(screen.contains("Explore(Find callers)"), "{screen}");
        // 只看列表行：主线程的状态行本身可能写着 Thinking。
        let rows: Vec<&str> = screen.lines().filter(|l| l.contains('◯')).collect();
        assert!(rows.iter().all(|row| !row.contains("Thinking")), "{screen}");
        // 流停住：Agent 行与列表行都看得到。
        notice(
            &child,
            ModelNotice::Stalled {
                retry_in_ms: 100_000,
            },
        );
        ui.wait("the stall in the row and the list", |screen| {
            screen.contains("✻ Waiting for API response · will retry in")
                && screen.contains("· Waiting for API response")
        })
        .await;
        // 子代理的回复正常收尾（修复线 F113：流在只有 thinking 时断开，主循环会重发同一个请求，provider-errors.md §3.3；
        // 这里测的是行与列表，不测断流）。
        finish_with_text(&child, "Found them.");
        drop(child);
        events(&last, text_response(&["callers found"]));
        drop(last);
        ui.wait_for("callers found").await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_background_agent_row_keeps_its_duration_at_the_right_edge() {
    let (model, senders) = NoticeModel::new(3);
    let mut senders: Vec<_> = senders.into_iter().map(Some).collect();
    let probe = model.clone();
    let setup = Setup {
        width: 100,
        height: 30,
        ..Setup::default()
    };
    with_ui(model.clone(), setup, |mut ui| async move {
        ui.submit("review in the background");
        events(
            senders[0].as_ref().unwrap(),
            tool_use_response(
                "toolu_agent",
                "Agent",
                &json!({"description": "Review code", "prompt": "Review it slowly",
                        "run_in_background": true}),
            ),
        );
        senders[0] = None;
        ui.wait_for("Backgrounded agent").await;
        // 主线程的下一次请求与子代理的请求谁先到不定：按 system 里的子代理权限句找出子代理那一个。
        let requests = loop {
            let requests = probe.requests.lock().unwrap().clone();
            if requests.len() == 3 {
                break requests;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        };
        let is_child = |index: usize| {
            requests[index].system.iter().any(|block| {
                block
                    .text
                    .contains("Messages from the agent that launched you")
            })
        };
        let (child, main) = if is_child(1) { (1, 2) } else { (2, 1) };
        events(
            senders[main].as_ref().unwrap(),
            text_response(&["started a review"]),
        );
        senders[main] = None;
        ui.wait_for("started a review").await;
        thinking(senders[child].as_ref().unwrap(), "Reading the diff");
        // 修复线 F26：列表行没有“思考中”；用时右对齐在行尾（原来接在活动前面，活动一变就左右跳）。
        let screen = ui
            .wait("the list row", |screen| {
                row_of(screen, "Review code").is_some()
            })
            .await;
        let row = row_of(&screen, "Review code").unwrap();
        assert!(!row.contains("Thinking"), "{screen}");
        assert!(ends_with_duration(&row), "{screen}");
        notice(
            senders[child].as_ref().unwrap(),
            ModelNotice::Retrying {
                attempt: 1,
                max_retries: 10,
                delay_ms: 60_000,
                error: ModelError::Overloaded {
                    message: "Overloaded".to_owned(),
                },
            },
        );
        let screen = ui.wait_for("· Retrying in").await;
        let retrying = row_of(&screen, "Review code").unwrap();
        assert!(retrying.contains("Review code · Retrying in"), "{screen}");
        assert!(ends_with_duration(&retrying), "{screen}");
        assert_eq!(
            unicode_width::UnicodeWidthStr::width(retrying.as_str()),
            unicode_width::UnicodeWidthStr::width(row.as_str()),
            "{screen}"
        );
        drop(senders);
    })
    .await;
}

/// 子代理列表里含 `needle` 的那一行。
fn row_of(screen: &str, needle: &str) -> Option<String> {
    screen
        .lines()
        .find(|line| line.contains('◯') && line.contains(needle))
        .map(str::to_owned)
}

/// 行尾是用时（`5s`、`1m 5s`）。
fn ends_with_duration(row: &str) -> bool {
    row.rsplit(' ')
        .next()
        .and_then(|last| last.strip_suffix('s'))
        .is_some_and(|digits| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
}
