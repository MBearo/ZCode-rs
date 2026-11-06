//! 子代理的权限询问（docs/specs/rust-cli/agents.md §4.1.1，修复线 F67）：内核是 LocalKernel，子代理由 agents 的
//! Agent 工具真实地派生。后台子代理的 Bash 在主回合结束之后才问：请求照样打开，回答回到子代理。样子照 Puffin 子代理的
//! 请求（tui.md §5.2，tui-kit.md §4.2）：这个会话里没有它的行，正文里画 `● Bash  命令`（在启动它的 Agent 行下面），预览的
//! 第一行是 `general-purpose asks`。模型按请求来源回复；子代理的第一步挂在闸门上，等主回合结束再放行。
use std::sync::{Arc, Mutex};

use crossterm::event::KeyCode;
use futures_util::stream::{self, BoxStream, StreamExt};
use rust_cli_kernel_api::model::{ModelPort, ModelRequest, QuerySource};
use rust_cli_kernel_api::testing::{text_response, tool_use_response};
use serde_json::json;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::support::{Item, Setup, request_text, with_ui};

/// 子代理被拒时 tool_result 的文字（主线程的拒绝文字是另一段）。
const SUBAGENT_DENIED: &str = "Permission for this tool use was denied. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). Try a different approach or report the limitation to complete your task.";

/// 主线程按序取剧本；子代理第一步调 Bash（挂在闸门上），之后回一段文字；旁路请求回固定文本。
struct Routed {
    main: Mutex<Vec<Vec<Item>>>,
    child: Mutex<Vec<ModelRequest>>,
    gate: (watch::Sender<bool>, watch::Receiver<bool>),
}

impl Routed {
    fn new(main: Vec<Vec<Item>>) -> Arc<Self> {
        Arc::new(Self {
            main: Mutex::new(main.into_iter().rev().collect()),
            child: Mutex::default(),
            gate: watch::channel(false),
        })
    }

    fn release(&self) {
        self.gate.0.send_replace(true);
    }

    /// 子代理发出的请求（按到达顺序）。
    fn child_requests(&self) -> Vec<ModelRequest> {
        self.child.lock().unwrap().clone()
    }
}

impl ModelPort for Routed {
    fn stream(&self, request: ModelRequest, cancel: CancellationToken) -> BoxStream<'static, Item> {
        let reply = match &request.source {
            QuerySource::MainThread => {
                let next = self.main.lock().unwrap().pop();
                stream::iter(next.unwrap_or_else(|| text_response(&["Noted."]))).boxed()
            }
            QuerySource::Subagent(_) => {
                let first = self.child.lock().unwrap().is_empty();
                self.child.lock().unwrap().push(request);
                if first {
                    let events = tool_use_response(
                        "toolu_child_bash",
                        "Bash",
                        &json!({"command": "touch made-by-child.txt", "description": "create a file"}),
                    );
                    let mut open = self.gate.1.clone();
                    stream::once(async move {
                        let _ = open.wait_for(|open| *open).await;
                    })
                    .flat_map(move |()| stream::iter(events.clone()))
                    .boxed()
                } else {
                    stream::iter(text_response(&["Child done."])).boxed()
                }
            }
            _ => stream::iter(text_response(&["Fixture side reply"])).boxed(),
        };
        reply.take_until(cancel.cancelled_owned()).boxed()
    }
}

fn spawn(background: bool, agent_type: &str) -> Vec<Item> {
    tool_use_response(
        "toolu_main_agent",
        "Agent",
        &json!({"description": "Make the file", "prompt": "Create the file.",
                "subagent_type": agent_type, "run_in_background": background}),
    )
}

/// 后台：派生、主线程回一句就结束回合；子代理结束后的通知回合回 `Noted.`。
fn background_model() -> Arc<Routed> {
    Routed::new(vec![
        spawn(true, "general-purpose"),
        text_response(&["Launched."]),
    ])
}

/// 等主回合结束（回复画出、footer 不再有 `esc stop`），再放行子代理的 Bash，等它的询问弹出。
async fn ask_after_the_turn(ui: &mut super::support::Ui, model: &Routed) -> String {
    ui.submit("make the file in the background");
    ui.wait("the main turn to end", |s| {
        s.contains("Launched.") && !s.contains("esc stop")
    })
    .await;
    model.release();
    let screen = ui.wait_for("Run this command?").await;
    ui.settle().await;
    screen
}

/// 子代理的请求自己的行（`● Bash  命令`）与预览的第一行（谁在问）。
fn asks_from(screen: &str, who: &str) {
    assert!(
        screen.contains("● Bash  touch made-by-child.txt"),
        "{screen}"
    );
    let rail = format!("│ {who}");
    assert!(screen.lines().any(|l| l.trim() == rail), "{rail}\n{screen}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_background_agent_asks_after_the_turn_and_runs_the_allowed_command() {
    let model = background_model();
    let script = model.clone();
    with_ui(model.clone(), Setup::default(), |mut ui| async move {
        let screen = ask_after_the_turn(&mut ui, &script).await;
        asks_from(&screen, "general-purpose asks");
        ui.key(KeyCode::Enter);
        // 子代理跑完，通知开了新回合。
        ui.wait_for("Noted.").await;
    })
    .await;
    let child = model.child_requests();
    assert_eq!(child.len(), 2, "{child:?}");
    // 允许后工具真的执行了（测试环境没有执行环境，结果是执行环境的错误，不是拒绝）。
    let second = request_text(&child[1]);
    assert!(second.contains("toolu_child_bash"), "{second}");
    assert!(
        !second.contains("Permission for this tool use was denied"),
        "{second}"
    );
    assert!(
        !second.contains("touch made-by-child.txt needs approval"),
        "{second}"
    );
}

/// 选 Deny（没有建议规则时是第 3 个回答）：子代理拿到子代理的拒绝文字。
#[tokio::test(flavor = "multi_thread")]
async fn denying_a_background_ask_gives_the_subagent_the_denial() {
    let model = background_model();
    let script = model.clone();
    with_ui(model.clone(), Setup::default(), |mut ui| async move {
        ask_after_the_turn(&mut ui, &script).await;
        ui.key(KeyCode::Char('3'));
        ui.wait_for("Noted.").await;
    })
    .await;
    let second = request_text(&model.child_requests()[1]);
    assert!(second.contains(SUBAGENT_DENIED), "{second}");
    assert!(!second.contains("doesn't want to proceed"), "{second}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_denial_with_feedback_reaches_the_subagent() {
    let model = background_model();
    let script = model.clone();
    with_ui(model.clone(), Setup::default(), |mut ui| async move {
        ask_after_the_turn(&mut ui, &script).await;
        ui.type_text("use make instead");
        ui.wait_for("deny and send").await;
        ui.key(KeyCode::Enter);
        ui.wait_for("Noted.").await;
    })
    .await;
    let second = request_text(&model.child_requests()[1]);
    assert!(
        second.contains("Permission for this tool use was denied. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). The user said:\\nuse make instead"),
        "{second}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_foreground_agent_ask_names_the_agent() {
    let model = Routed::new(vec![
        spawn(false, "general-purpose"),
        text_response(&["Main done."]),
    ]);
    model.release();
    // 前台子代理只在 fork 关着时有（`ZCODE_FORK_SUBAGENT=0`，修复线 L-A，agents.md §8.1.1）。
    let setup = Setup {
        fork_subagent: Some(false),
        ..Setup::default()
    };
    with_ui(model.clone(), setup, |mut ui| async move {
        ui.submit("make the file");
        let screen = ui.wait_for("Run this command?").await;
        asks_from(&screen, "general-purpose asks");
        ui.settle().await;
        ui.key(KeyCode::Char('3'));
        ui.wait_for("Main done.").await;
    })
    .await;
    // 前台子代理被拒也是子代理的文字。
    let second = request_text(&model.child_requests()[1]);
    assert!(second.contains(SUBAGENT_DENIED), "{second}");
}

#[tokio::test(flavor = "multi_thread")]
async fn chinese_names_the_agent_in_the_title() {
    let model = background_model();
    let script = model.clone();
    let setup = Setup {
        lang: "zh_CN.UTF-8",
        ..Setup::default()
    };
    with_ui(model.clone(), setup, |mut ui| async move {
        ui.submit("在后台建文件");
        ui.wait("the main turn to end", |s| {
            s.contains("Launched.") && !s.contains("esc 停止") && !s.contains("esc stop")
        })
        .await;
        script.release();
        let screen = ui.wait_for("运行这条命令？").await;
        asks_from(&screen, "general-purpose 在问");
        ui.settle().await;
        ui.key(KeyCode::Enter);
        ui.wait_for("Noted.").await;
    })
    .await;
}

/// 等子代理发出第 `count` 个请求（它的通知留在旧会话，画面上没有可等的文字）。
async fn child_requests(model: &Routed, count: usize) {
    let wait = async {
        while model.child_requests().len() < count {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    };
    let waited = tokio::time::timeout(std::time::Duration::from_secs(10), wait).await;
    assert!(
        waited.is_ok(),
        "timed out waiting for the subagent's next request"
    );
}

/// `/clear` 之后旧会话的后台子代理照样询问（agents.md §4.1.2，工作线 F72）：对话框标出会话与子代理，允许后子代理
/// 执行。修复前父会话关闭后询问发不出去，子代理按拒绝处理。
#[tokio::test(flavor = "multi_thread")]
async fn a_background_agent_of_a_cleared_session_still_asks() {
    let model = background_model();
    let script = model.clone();
    let wide = Setup {
        width: 120,
        ..Setup::default()
    };
    with_ui(model.clone(), wide, |mut ui| async move {
        ui.submit("make the file in the background");
        ui.wait("the main turn to end", |s| {
            s.contains("Launched.") && !s.contains("esc stop")
        })
        .await;
        ui.submit("/clear");
        ui.wait("the cleared conversation", |s| !s.contains("Launched."))
            .await;
        script.release();
        let screen = ui.wait_for("Run this command?").await;
        // 旧会话的请求：正文末尾自己的行，预览的第一行带上会话。
        asks_from(
            &screen,
            "general-purpose asks in session \"make the file in the background\"",
        );
        ui.settle().await;
        ui.key(KeyCode::Enter);
        ui.wait("the request to leave", |s| !s.contains("Run this command?"))
            .await;
        child_requests(&script, 2).await;
    })
    .await;
    let second = request_text(&model.child_requests()[1]);
    assert!(second.contains("toolu_child_bash"), "{second}");
    assert!(
        !second.contains("Permission for this tool use was denied"),
        "{second}"
    );
}

/// 旧会话的询问选 Deny：子代理拿到子代理的拒绝文字。
#[tokio::test(flavor = "multi_thread")]
async fn denying_an_ask_of_a_cleared_session() {
    let model = background_model();
    let script = model.clone();
    with_ui(model.clone(), Setup::default(), |mut ui| async move {
        ui.submit("make the file in the background");
        ui.wait("the main turn to end", |s| {
            s.contains("Launched.") && !s.contains("esc stop")
        })
        .await;
        ui.submit("/clear");
        ui.wait("the cleared conversation", |s| !s.contains("Launched."))
            .await;
        script.release();
        ui.wait_for("Run this command?").await;
        ui.settle().await;
        ui.key(KeyCode::Char('3'));
        child_requests(&script, 2).await;
    })
    .await;
    let second = request_text(&model.child_requests()[1]);
    assert!(second.contains(SUBAGENT_DENIED), "{second}");
}
