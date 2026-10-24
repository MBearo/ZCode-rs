//! 入口收到退出信号（R5-2，docs/specs/rust-cli/session-lifecycle.md §3.5）：界面中止正在跑的回合，等它结束后
//! 离开（SessionEnd 的原因是 `other`）；输入一直没有结束，事件循环只能经这个信号退出。中止按 `Shutdown`（修复线
//! F38）：TUI 收到退出信号不中止 query，之后这个回合不再记录。
use std::sync::Arc;
use std::time::Duration;

use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::message::ContentBlock;
use rust_cli_kernel_api::model::{BlockDelta, ModelEvent};
use rust_cli_kernel_api::testing::{MemoryStore, NoticeEnvironment, environments};
use tokio_util::sync::CancellationToken;

use super::support::{GatedModel, Setup, send, with_ui, with_ui_exit};

#[tokio::test(flavor = "multi_thread")]
async fn a_stop_signal_interrupts_the_running_turn_and_leaves() {
    let (model, senders) = GatedModel::new(1);
    let first = senders.into_iter().next().unwrap();
    // 响应一直不结束：回合只能被中止（发送端留到最后才释放）。
    let keep = first.clone();
    let stop = CancellationToken::new();
    let store = Arc::new(MemoryStore::default());
    let setup = Setup {
        stop: Some(stop.clone()),
        store: store.clone(),
        ..Setup::default()
    };
    let run = with_ui(model, setup, |mut ui| async move {
        ui.submit("long task");
        send(
            &first,
            vec![
                Ok(ModelEvent::MessageStart {
                    id: "msg_api".to_owned(),
                    model: "scripted".to_owned(),
                    usage: Default::default(),
                }),
                Ok(ModelEvent::BlockStart {
                    index: 0,
                    block: ContentBlock::Text {
                        text: String::new(),
                    },
                }),
                // 带换行：流式中没写完的最后一行不显示（`cells::messages::visible`）。
                Ok(ModelEvent::BlockDelta {
                    index: 0,
                    delta: BlockDelta::Text("partial reply\n".to_owned()),
                }),
            ],
        );
        // 修复线 F35：等模型流出的文字，不等 `esc to interrupt`。后者在 TurnStarted 时就画出，输入要等
        // UserPromptSubmit 跑完才提交；信号落在这段空档里时回合在提交输入之前中止，库里既没有输入也没有中断标记
        // （session-lifecycle.md §3.1 的空档）。流出的文字说明输入已提交、主循环在跑。
        let running = ui.wait_for("partial reply").await;
        assert!(running.contains("esc to interrupt"), "{running}");
        stop.cancel();
    });
    // 期限只用于让失败的测试结束。
    let session = tokio::time::timeout(Duration::from_secs(10), run)
        .await
        .expect("the loop left on the stop signal")
        .expect("a session was created");
    drop(keep);
    let facts = format!("{:?}", store.facts(&SessionId::new(session)));
    // 回合在离开之前结束（回合计量记 Cancelled），输入留着；没有中断标记、没有流出的半句（F37 时写中断标记）。
    assert!(facts.contains("long task"), "{facts}");
    assert!(
        facts.contains("Cancelled"),
        "the running turn ended before leaving: {facts}"
    );
    assert!(
        !facts.contains("[Request interrupted by user]") && !facts.contains("partial reply"),
        "nothing is recorded after the exit signal: {facts}"
    );
}

/// F14：退出的顺序是先恢复终端、打印恢复提示，再跑 SessionEnd（session-lifecycle.md §3.5）。终端
/// 在 `tui::run` 返回前恢复，所以事件循环返回时会话还不能关：SessionEnd 留给入口在打印恢复提示之后跑。修前事件循环
/// 自己先关会话（SessionEnd 在恢复终端之前）。
#[tokio::test(flavor = "multi_thread")]
async fn the_session_ends_only_after_the_loop_hands_the_exit_back() {
    let (model, senders) = GatedModel::new(1);
    let first = senders.into_iter().next().unwrap();
    let keep = first.clone();
    let stop = CancellationToken::new();
    let env = Arc::new(NoticeEnvironment::default());
    let setup = Setup {
        stop: Some(stop.clone()),
        environments: environments(env.clone()),
        ..Setup::default()
    };
    let run = with_ui_exit(model, setup, |mut ui| async move {
        ui.submit("long task");
        send(
            &first,
            vec![Ok(ModelEvent::MessageStart {
                id: "msg_api".to_owned(),
                model: "scripted".to_owned(),
                usage: Default::default(),
            })],
        );
        ui.wait_for("esc to interrupt").await;
        stop.cancel();
    });
    // 期限只用于让失败的测试结束。
    let exit = tokio::time::timeout(Duration::from_secs(10), run)
        .await
        .expect("the loop left on the stop signal");
    drop(keep);
    let session = exit.session.clone().expect("a session was created");
    assert!(
        env.closed().is_empty(),
        "SessionEnd ran before the terminal could be restored: {:?}",
        env.closed()
    );
    exit.ending.run().await;
    let closed: Vec<SessionId> = env.closed().into_iter().map(|(id, _)| id).collect();
    assert_eq!(closed, vec![SessionId::new(session)]);
}
