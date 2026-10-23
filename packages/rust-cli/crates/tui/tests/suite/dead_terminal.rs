//! 终端关掉之后的画帧（F14，docs/specs/rust-cli/session-lifecycle.md §3.5）：真实的终端关闭（关窗口、SSH 断开）
//! 先让写终端失败（EIO），SIGHUP 随后才到；修前 `draw()?` 直接返回错误，回合不中止、SessionEnd 不跑。现在画不出来
//! 就按收到退出信号收尾。终端是包了一层的 `TestBackend`：打开开关后每次画帧都失败。
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ratatui::backend::{Backend, ClearType, TestBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::message::ContentBlock;
use rust_cli_kernel_api::model::{BlockDelta, ModelEvent};
use rust_cli_kernel_api::testing::{MemoryStore, NoticeEnvironment};
use tokio_util::sync::CancellationToken;

use super::support::{GatedModel, Setup, send, with_ui_exit};

/// 打开 `dead` 之后画帧失败的终端（其余操作照常交给 `TestBackend`）。
pub struct Hangable {
    pub inner: TestBackend,
    pub dead: Arc<AtomicBool>,
}

fn io<T>(result: Result<T, core::convert::Infallible>) -> std::io::Result<T> {
    result.map_err(|never| match never {})
}

impl Backend for Hangable {
    type Error = std::io::Error;

    fn draw<'a, I>(&mut self, content: I) -> std::io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        if self.dead.load(Ordering::SeqCst) {
            return Err(std::io::Error::from_raw_os_error(5));
        }
        io(self.inner.draw(content))
    }

    fn hide_cursor(&mut self) -> std::io::Result<()> {
        io(self.inner.hide_cursor())
    }

    fn show_cursor(&mut self) -> std::io::Result<()> {
        io(self.inner.show_cursor())
    }

    fn get_cursor_position(&mut self) -> std::io::Result<Position> {
        io(self.inner.get_cursor_position())
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> std::io::Result<()> {
        io(self.inner.set_cursor_position(position))
    }

    fn clear(&mut self) -> std::io::Result<()> {
        io(self.inner.clear())
    }

    fn clear_region(&mut self, clear_type: ClearType) -> std::io::Result<()> {
        io(self.inner.clear_region(clear_type))
    }

    fn size(&self) -> std::io::Result<Size> {
        io(self.inner.size())
    }

    fn window_size(&mut self) -> std::io::Result<WindowSize> {
        io(self.inner.window_size())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        io(self.inner.flush())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dead_terminal_ends_the_session_like_an_exit_signal() {
    let (model, senders) = GatedModel::new(1);
    let first = senders.into_iter().next().unwrap();
    // 响应一直不结束：回合只能被中止（发送端留到最后才释放）。
    let keep = first.clone();
    let stop = CancellationToken::new();
    let store = Arc::new(MemoryStore::default());
    let env = Arc::new(NoticeEnvironment::default());
    let hang = Arc::new(AtomicBool::new(false));
    let setup = Setup {
        stop: Some(stop),
        store: store.clone(),
        env: env.clone(),
        hang: hang.clone(),
        ..Setup::default()
    };
    let run = with_ui_exit(model, setup, |mut ui| async move {
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
        // 修复线 F35：等模型流出的文字（输入已提交、主循环在跑），不等 `esc to interrupt`（TurnStarted 时就画出，
        // 输入要等 UserPromptSubmit 跑完才提交；终端在这段空档里没了时，回合在提交输入之前中止）。
        ui.wait_for("partial reply").await;
        // 终端没了；下一帧（这条增量带来的）画不出来。退出信号没有来（stop 令牌不取消）。
        hang.store(true, Ordering::SeqCst);
        send(
            &first,
            vec![Ok(ModelEvent::BlockDelta {
                index: 0,
                delta: BlockDelta::Text("more reply\n".to_owned()),
            })],
        );
    });
    // 期限只用于让失败的测试结束。
    let exit = tokio::time::timeout(std::time::Duration::from_secs(10), run)
        .await
        .expect("the loop left after the terminal died");
    drop(keep);
    let session = exit.session.clone().expect("a session was created");
    let facts = format!("{:?}", store.facts(&SessionId::new(session.clone())));
    // 回合被中止并结束（回合计量记 Cancelled）；按退出信号（`Shutdown`，修复线 F38，收到 SIGHUP 不中止 query）
    // 不写中断标记、不留流出的半句。F37 时写中断标记。
    assert!(
        facts.contains("Cancelled"),
        "the running turn ended: {facts}"
    );
    assert!(
        !facts.contains("[Request interrupted by user]") && !facts.contains("partial reply"),
        "nothing is recorded after the exit: {facts}"
    );
    exit.ending.run().await;
    let closed: Vec<SessionId> = env.closed().into_iter().map(|(id, _)| id).collect();
    assert_eq!(closed, vec![SessionId::new(session)]);
}
