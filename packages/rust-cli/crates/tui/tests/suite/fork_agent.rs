//! fork 子代理（修复线 L-A 的 A1，docs/specs/rust-cli/agents.md §8.1）：交互会话里 Agent 用 fork 写法；
//! `subagent_type: "fork"` 的子代理继承父会话的对话、system 与工具集，总在后台，结束后通知开新回合。
use std::sync::{Arc, Mutex};

use futures_util::stream::{self, BoxStream, StreamExt};
use rust_cli_kernel_api::model::{ModelError, ModelEvent, ModelPort, ModelRequest};
use rust_cli_kernel_api::testing::{text_response, tool_use_response};
use tokio_util::sync::CancellationToken;

use super::support::{Setup, request_text, with_ui};

const BOILERPLATE: &str = "<fork-boilerplate>";

/// 按请求回答：fork 子代理的请求（带 boilerplate）回报告；主线程按顺序：派生 fork、回一句、通知回合。
///
/// 修复测试的时序：fork 若在主线程的第二个请求之前结束，通知在工具边界跟进那个请求（不开新回合，行为正确），测试
/// 却等第三个请求，负载高时约三成失败。现在 fork 的回答等主线程的第二个请求发出之后才给，通知总在回合结束后开新回合。
#[derive(Default)]
struct Router {
    main: Mutex<usize>,
    requests: Mutex<Vec<ModelRequest>>,
    /// 主线程的第二个请求发出时放行 fork 的回答（`notify_one` 先到时留一个许可）。
    second: Arc<tokio::sync::Notify>,
}

impl ModelPort for Router {
    fn stream(
        &self,
        request: ModelRequest,
        _cancel: CancellationToken,
    ) -> BoxStream<'static, Result<ModelEvent, ModelError>> {
        let fork = request_text(&request).contains(BOILERPLATE);
        self.requests.lock().unwrap().push(request);
        if fork {
            let second = self.second.clone();
            return stream::once(async move { second.notified().await })
                .flat_map(|()| stream::iter(text_response(&["Fork report: alpha"])))
                .boxed();
        }
        let mut main = self.main.lock().unwrap();
        *main += 1;
        let events = match *main {
            1 => tool_use_response(
                "toolu_fork",
                "Agent",
                &serde_json::json!({
                    "description": "Read the file",
                    "prompt": "Read a.txt and report its first line.",
                    "subagent_type": "fork",
                }),
            ),
            2 => {
                self.second.notify_one();
                text_response(&["Forked."])
            }
            _ => text_response(&["Noted the fork result."]),
        };
        stream::iter(events).boxed()
    }
}

fn idle(screen: &str, text: &str) -> bool {
    screen.contains(text) && !screen.contains("esc to interrupt")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fork_inherits_the_conversation_and_reports_back() {
    let model = Arc::new(Router::default());
    let setup = Setup {
        width: 120,
        height: 40,
        ..Setup::default()
    };
    with_ui(model.clone(), setup, |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("Fork a worker to read a.txt.");
        ui.wait("the notification turn", |s| {
            idle(s, "Noted the fork result.")
        })
        .await;
    })
    .await;
    let requests = model.requests.lock().unwrap().clone();
    let (forks, mains): (Vec<_>, Vec<_>) = requests
        .iter()
        .partition(|request| request_text(request).contains(BOILERPLATE));
    assert_eq!(forks.len(), 1, "{requests:#?}");
    assert_eq!(mains.len(), 3, "{requests:#?}");
    let (first, child) = (mains[0], forks[0]);
    // 主线程的 Agent 是 fork 写法：没有 `run_in_background`，描述里有 fork 一段。
    let agent = first
        .tools
        .iter()
        .find(|tool| tool.name == "Agent")
        .unwrap();
    assert!(
        agent.description.contains("`\"fork\"` forks yourself"),
        "{}",
        agent.description
    );
    assert!(
        agent.input_schema["properties"]
            .get("run_in_background")
            .is_none()
    );
    // 子代理：system 与工具集同主线程，消息是父的历史加只带这个调用的 assistant 与“已启动”结果、directive。
    assert_eq!(child.system, first.system);
    assert_eq!(
        child
            .tools
            .iter()
            .map(|tool| &tool.name)
            .collect::<Vec<_>>(),
        first
            .tools
            .iter()
            .map(|tool| &tool.name)
            .collect::<Vec<_>>()
    );
    // 第一条消息相同（缓存断点的位置随请求不同，不比）。
    let blocks = |request: &ModelRequest| {
        request.messages[0]
            .content
            .iter()
            .map(|item| item.block.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(blocks(child), blocks(first));
    let text = request_text(child);
    assert!(
        text.contains("Fork started — processing in background"),
        "{text}"
    );
    assert!(
        text.contains("Your directive: Read a.txt and report its first line."),
        "{text}"
    );
    // 父的结果是“已启动”，结束后通知开新回合。
    assert!(request_text(mains[1]).contains("Async agent launched successfully."));
    assert!(request_text(mains[2]).contains("<task-notification>"));
}
