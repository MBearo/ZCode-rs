//! 消息区的显示（docs/specs/rust-cli/tui-display.md §2）：前台子代理的进度与 Done 行、后台子代理与 footer 的
//! pill、Background 面板（`/tasks`）的停止、任务清单与 ctrl+t。内核是 LocalKernel（agents 的 Agent、TaskCreate 工具）。
use crossterm::event::{KeyCode, KeyModifiers};
use rust_cli_kernel_api::testing::{text_response, tool_use_response};
use serde_json::json;

use super::support::{GatedModel, Setup, finish, request_text, send, with_ui};

/// 子代理的请求（system 里有子代理的权限句）。
fn is_subagent(request: &rust_cli_kernel_api::model::ModelRequest) -> bool {
    request.system.iter().any(|block| {
        block
            .text
            .contains("Messages from the agent that launched you")
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn a_foreground_agent_shows_its_progress_and_then_done() {
    let (model, senders) = GatedModel::new(3);
    let mut senders = senders.into_iter();
    let (main, child, last) = (
        senders.next().unwrap(),
        senders.next().unwrap(),
        senders.next().unwrap(),
    );
    // 前台子代理只在 fork 关着时有（`ZCODE_FORK_SUBAGENT=0`，修复线 L-A，agents.md §8.1.1）。
    let setup = Setup {
        fork_subagent: Some(false),
        ..Setup::default()
    };
    with_ui(model.clone(), setup, |mut ui| async move {
        ui.submit("find the callers");
        finish(
            main,
            tool_use_response(
                "toolu_agent",
                "Agent",
                &json!({"description": "Find callers", "prompt": "Look around",
                        "subagent_type": "Explore", "run_in_background": false}),
            ),
        );
        ui.wait_for("Initializing…").await;
        let mut reply = text_response(&["Checking files"]);
        let tail = reply.split_off(3);
        send(&child, reply);
        let screen = ui.wait_for("⎿  Checking files").await;
        assert!(screen.contains("Explore(Find callers)"), "{screen}");
        finish(child, tail);
        finish(last, text_response(&["callers found"]));
        let screen = ui.wait_for("callers found").await;
        assert!(
            screen.contains("⎿  Done (0 tool uses · 0 tokens · 0s)"),
            "{screen}"
        );
    })
    .await;
    assert!(is_subagent(&model.requests()[1]));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_background_agent_shows_in_the_footer_and_stops_from_tasks() {
    let (model, senders) = GatedModel::new(4);
    let mut senders: Vec<_> = senders.into_iter().map(Some).collect();
    let probe = model.clone();
    with_ui(model.clone(), Setup::default(), |mut ui| async move {
        ui.submit("review in the background");
        finish(
            senders[0].take().unwrap(),
            tool_use_response(
                "toolu_agent",
                "Agent",
                &json!({"description": "Review code", "prompt": "Review it slowly"}),
            ),
        );
        ui.wait_for("Backgrounded agent (↓ to manage · ctrl+o to expand)")
            .await;
        // 主线程的下一次请求与子代理的请求谁先到不定：按请求内容找出主线程那一个，先让它结束。
        let requests = loop {
            let requests = probe.requests();
            if requests.len() == 3 {
                break requests;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        };
        let main = if is_subagent(&requests[1]) { 2 } else { 1 };
        finish(
            senders[main].take().unwrap(),
            text_response(&["started a review"]),
        );
        let screen = ui
            .wait("the pill", |s| {
                s.contains("· 1 local agent") && !s.contains("esc to interrupt")
            })
            .await;
        assert!(screen.contains("↓ to manage"), "{screen}");
        // ↓ 先选中 pill，enter 打开 Background 面板（tui-panels.md §3.1）。
        ui.key(KeyCode::Down);
        // 80 列时提示放不下会被截断（`Enter to view tas…`）。
        ui.wait_for("· 1 local agent · Enter to view").await;
        ui.key(KeyCode::Enter);
        let list = ui.wait_for("1 active agent").await;
        assert!(list.contains("  Background"), "{list}");
        assert!(
            list.lines()
                .any(|l| l.contains("Review code") && l.ends_with("running")),
            "{list}"
        );
        ui.key(KeyCode::Char('x'));
        ui.wait_for("Stopped Review code").await;
        // 停止的通知开了一个回合（task-notification），pill 随之消失。
        finish(
            senders[3].take().unwrap(),
            text_response(&["review stopped"]),
        );
        ui.key(KeyCode::Esc);
        ui.wait("the pill to go away", |s| {
            s.contains("review stopped") && !s.contains("1 local agent")
        })
        .await;
        drop(senders);
    })
    .await;
    let requests = model.requests();
    assert!(request_text(requests.last().unwrap()).contains("task-notification"));
}

#[tokio::test(flavor = "multi_thread")]
async fn task_list_shows_above_the_input_and_ctrl_t_toggles_it() {
    let (model, senders) = GatedModel::new(2);
    let mut senders = senders.into_iter();
    let (first, second) = (senders.next().unwrap(), senders.next().unwrap());
    with_ui(model, Setup::default(), |mut ui| async move {
        ui.submit("plan the work");
        finish(
            first,
            tool_use_response(
                "toolu_task",
                "TaskCreate",
                &json!({"subject": "Write the tests", "description": "cover the list"}),
            ),
        );
        finish(second, text_response(&["planned"]));
        let screen = ui.wait_for("1 tasks (0 done, 1 open)").await;
        assert!(screen.contains("◻ Write the tests"), "{screen}");
        let screen = ui
            .wait("the turn to end", |s| {
                s.contains("planned") && !s.contains("esc to interrupt")
            })
            .await;
        assert!(screen.contains("ctrl+t to hide tasks"), "{screen}");
        ui.press(KeyCode::Char('t'), KeyModifiers::CONTROL);
        let screen = ui
            .wait("the list to hide", |s| !s.contains("1 tasks (0 done"))
            .await;
        assert!(screen.contains("ctrl+t to show tasks"), "{screen}");
        ui.press(KeyCode::Char('t'), KeyModifiers::CONTROL);
        ui.wait_for("1 tasks (0 done, 1 open)").await;
    })
    .await;
}
