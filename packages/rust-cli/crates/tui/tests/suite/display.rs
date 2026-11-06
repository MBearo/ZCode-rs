//! 消息区的显示（docs/specs/rust-cli/tui-display.md §2）：前台子代理的进度与 Done 行、后台子代理与任务行、^T 与 `/tasks`
//! 打开的任务卡（tui-tasks.md）与在卡里停止、任务清单与 ^O 里的整个清单。内核是 LocalKernel（agents 的 Agent、TaskCreate 工具）。
use crossterm::event::{KeyCode, KeyModifiers};
use rust_cli_kernel_api::testing::{text_response, tool_use_response};
use serde_json::json;

use super::agents_tree::list;
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
        // 在跑的子代理是一行调用：点阵、`Agent` 与它的描述（Puffin `tool_row::agent_row`）。以前下面另有
        // `Initializing…` 与子代理说的话（`│ Checking files`），Puffin 只写它此刻的调用。
        ui.wait_for("Agent  Find callers").await;
        let mut reply = text_response(&["Checking files"]);
        let tail = reply.split_off(3);
        send(&child, reply);
        finish(child, tail);
        finish(last, text_response(&["callers found"]));
        // 结束之后这一段工作收成一句（Puffin 的收拢）；以前是 `│ Done (0 tool uses · 0 tokens · 0s)`。
        let screen = ui.wait_for("callers found").await;
        assert!(screen.contains("✓ Ran 1 subagent"), "{screen}");
        assert!(!screen.contains("Checking files"), "{screen}");
    })
    .await;
    assert!(is_subagent(&model.requests()[1]));
}

/// 任务行与任务卡（tui-tasks.md §2、§3，用户决定 2026-10-31）：在跑的后台子代理只在输入框上方的一行里数，右沿 `^T all
/// tasks`；^T 打开任务卡，再按 ^T 或 esc 关上；`/tasks` 也打开它，`^X` 停止选中的子代理。状态栏里没有 pill、没有 `↓`。
#[tokio::test(flavor = "multi_thread")]
async fn a_background_agent_is_counted_and_stops_from_the_card() {
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
        // 后台子代理的调用马上结束：这一段工作收成 `Ran 1 subagent`（Puffin 的收拢），以前是
        // `Backgrounded agent (↓ to manage · ctrl+o to expand)`。
        ui.wait_for("Ran 1 subagent").await;
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
        // 回合结束后：状态栏里没有 pill，也没有 `↓ to manage`；状态栏下面没有常驻的子代理列表。
        let screen = ui
            .wait("the turn to end", |s| {
                s.contains("started a review") && !s.contains("esc stop")
            })
            .await;
        assert!(!screen.contains("local agent"), "{screen}");
        assert!(!screen.contains("· ↓ to manage"), "{screen}");
        assert!(list(&screen).is_empty(), "{screen}");
        // 任务行：只数，接着是打开卡的键 `↓/^T`（tui-tasks.md §2.2）。
        let screen = ui.wait_for(" ○ 1 subagent running ").await;
        let row = screen
            .lines()
            .find(|l| l.starts_with(" ○ 1 subagent running "))
            .unwrap();
        assert!(row.contains(" running   ↓/^T all tasks"), "{screen}");
        // ^T 打开任务卡，再按 ^T 关上；esc 也关上。
        for close in [
            (KeyCode::Char('t'), KeyModifiers::CONTROL),
            (KeyCode::Esc, KeyModifiers::NONE),
        ] {
            ui.press(KeyCode::Char('t'), KeyModifiers::CONTROL);
            let card = ui
                .wait("the subagent row", |s| {
                    list(s)
                        .iter()
                        .any(|l| l.starts_with("   ○ general-purpose  Review code"))
                })
                .await;
            assert!(
                card.contains("   Subagents 1  ·  Commands  ·  Todos"),
                "{card}"
            );
            assert!(!card.contains(" ○ 1 subagent running "), "{card}");
            ui.press(close.0, close.1);
            ui.wait("the card to close", |s| list(s).is_empty()).await;
        }
        // `/tasks` 也打开它；选中子代理按 ^X 停止（与 TaskStop 同一条路径）。
        ui.submit("/tasks");
        ui.wait("the subagent row", |s| {
            list(s)
                .iter()
                .any(|l| l.starts_with("   ○ general-purpose  Review code"))
        })
        .await;
        ui.key(KeyCode::Down);
        ui.wait_for("⏎ view").await;
        ui.press(KeyCode::Char('x'), KeyModifiers::CONTROL);
        ui.wait_for("Stopped Review code").await;
        // 停止的通知开了一个回合（task-notification）。
        finish(
            senders[3].take().unwrap(),
            text_response(&["review stopped"]),
        );
        ui.key(KeyCode::Esc);
        ui.wait("the turn of the notification", |s| {
            s.contains("review stopped") && !s.contains("esc stop")
        })
        .await;
        drop(senders);
    })
    .await;
    let requests = model.requests();
    assert!(request_text(requests.last().unwrap()).contains("task-notification"));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_task_line_counts_todos_and_the_card_lists_them() {
    let (model, senders) = GatedModel::new(2);
    let mut senders = senders.into_iter();
    let (first, second) = (senders.next().unwrap(), senders.next().unwrap());
    with_ui(model, Setup::default(), |mut ui| async move {
        // 会话里还没有任务：^T 在卡的位置说一句，到下一个键（tui-tasks.md §3.1）。
        ui.press(KeyCode::Char('t'), KeyModifiers::CONTROL);
        ui.wait_for("No tasks in this session.").await;
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
        // 任务行只数待办（tui-tasks.md §2）：`0 of 1 todos done`，步骤不在这里。
        let screen = ui.wait_for(" ○ 0 of 1 todos done ").await;
        assert!(!screen.contains("Write the tests"), "{screen}");
        // 回合结束后清单还是这个回合写的：任务行留着。
        ui.wait("the turn to end", |s| {
            s.contains("planned") && !s.contains("esc stop") && s.contains("0 of 1 todos done")
        })
        .await;
        // ^T：只有待办，任务卡落在 Todos：整个清单，右沿 `0 of 1 done`。
        ui.press(KeyCode::Char('t'), KeyModifiers::CONTROL);
        let card = ui.wait_for("   · Write the tests").await;
        assert!(
            card.contains("   Subagents  ·  Commands  ·  Todos 0/1"),
            "{card}"
        );
        assert!(card.contains("0 of 1 done"), "{card}");
        // ^O 不是卡的键：关上卡，照常处理。整个清单挂在最新的任务调用下面（tui-display.md §2.2，Puffin「^O 的 `Todos`
        // 行」）。
        ui.press(KeyCode::Char('o'), KeyModifiers::CONTROL);
        let steps = |s: &str| s.lines().filter(|l| l.contains("Write the tests")).count();
        let screen = ui
            .wait("the list under the call", |s| {
                !s.contains("   · Write the tests") && steps(s) == 1
            })
            .await;
        assert!(screen.contains("0 of 1 todos done"), "{screen}");
        // 再按 ^O 收起。
        ui.press(KeyCode::Char('o'), KeyModifiers::CONTROL);
        ui.wait("the list to fold", |s| steps(s) == 0).await;
    })
    .await;
}
