//! 会话生命周期的界面操作（docs/specs/rust-cli/session-lifecycle.md §5.4、§6.2）：rewind 选择器、
//! `/resume`、启动时打开会话、离开会话时的 SessionEnd。每个操作是一个 future，结果作为
//! [`Done`] 回到事件循环；状态只在事件循环里改。
use std::sync::Arc;

use futures_util::FutureExt;
use ratatui::backend::Backend;
use rust_cli_kernel_api::ids::{MessageId, SessionId};
use rust_cli_kernel_api::session_api::{
    Command, CommandEnvelope, EndReason, Kernel, KernelError, RewindScope, SessionHandle,
    SessionView, StartSource,
};
use rust_cli_kernel_api::store::SessionFilter;

use super::session::{Done, Sent};
use super::{Loop, idle};
use crate::ui::rewind::Entry;
use crate::view::Transcript;

/// 启动时要打开的会话（`--resume` / `-c`，tui.md §6）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Startup {
    #[default]
    New,
    /// `--resume <id>`。
    Session(String),
    /// `-c` / `--continue`：本工作区最近更新的会话。
    Latest,
    /// `--resume` 不带参数：打开会话选择器。
    Pick,
}

/// 切过去的会话：句柄与快照；`ended` 是旧会话 SessionEnd 给出的提示。
pub(super) struct Switched {
    pub handle: Arc<dyn SessionHandle>,
    pub view: SessionView,
    pub notice: Option<String>,
    pub ended: Vec<String>,
}

/// 打开会话并取快照；给了 `fork`（`Command::Fork` 的命令）时先在它的末尾 fork，再打开 fork 出的会话
/// （`--fork-session`）。给了 `store`（`Command::StoreResumeSentinel`）时取快照之前先发它。命令由调用方经注入的 Ids
/// 建好（F7）。
async fn open(
    kernel: Arc<dyn Kernel>,
    id: SessionId,
    source: StartSource,
    fork: Option<CommandEnvelope>,
    store: Option<CommandEnvelope>,
) -> Result<(Arc<dyn SessionHandle>, SessionView), KernelError> {
    let mut handle = kernel.open_session_as(&id, source).await?;
    if let Some(command) = fork {
        let ack = handle.send(command).await;
        let rust_cli_kernel_api::session_api::Ack::Accepted { result } = ack else {
            return Err(KernelError::Failed {
                message: format!("{ack:?}"),
            });
        };
        let child = SessionId::new(result.as_str().unwrap_or_default());
        handle = kernel.open_session_as(&child, StartSource::Fork).await?;
    }
    // 会话里切过去（`/resume`、`/fork`）：对话已经换了，续写时补的占位马上落库（session-lifecycle.md §6.3.1，工作线
    // F79）。回执不看：提交失败时它留在会话里，随下一批带消息的提交写。
    if let Some(command) = store {
        let _ = handle.send(command).await;
    }
    let view = handle.view().await?;
    Ok((handle, view))
}

impl<B: Backend> Loop<'_, B> {
    /// 启动时：按参数打开会话或会话选择器。
    pub(super) fn startup(&mut self, startup: Startup, fork: bool) {
        let kernel = self.kernel.clone();
        let filter = SessionFilter {
            workspace: self.app.view.workspace().clone(),
            limit: 1,
        };
        let id = match startup {
            Startup::New => return,
            Startup::Pick => return self.open_resume(true),
            Startup::Session(id) => Some(SessionId::new(id)),
            Startup::Latest => None,
        };
        let fork = fork.then(|| self.envelope(Command::Fork { at: None }));
        self.pending.push(
            async move {
                let opened = async {
                    let id = match id {
                        Some(id) => id,
                        None => match kernel.list_sessions(filter).await?.into_iter().next() {
                            Some(summary) => summary.id,
                            None => {
                                return Err(KernelError::Failed {
                                    message: "No conversation found to continue".to_owned(),
                                });
                            }
                        },
                    };
                    // 启动时打开（`--resume <id>`、`-c`）不发续写占位的命令：等第一处改动再写（§6.3.1）。
                    open(kernel, id, StartSource::Resume, fork, None).await
                };
                Done::Switched(Box::new(opened.await.map(|(handle, view)| Switched {
                    handle,
                    view,
                    notice: None,
                    ended: Vec::new(),
                })))
            }
            .boxed(),
        );
    }

    /// 不再看当前会话：停止接收它的事件与面板通知，交出句柄（由调用方关闭）。
    fn detach(&mut self) -> Option<Arc<dyn SessionHandle>> {
        let session = self.session.take()?;
        self.events = idle();
        self.detach_panels();
        Some(session)
    }

    /// 离开当前会话：运行 SessionEnd 并关闭它，提示随 [`Done::Notices`] 回来。
    pub(super) fn leave(&mut self, reason: EndReason) {
        let Some(session) = self.detach() else {
            return;
        };
        self.pending
            .push(async move { Done::Notices(session.close(reason).await) }.boxed());
    }

    /// 切到另一个会话（`/resume`、`/fork`）：旧会话 SessionEnd(resume)，新会话从快照重建。先关旧会话再开新会话
    /// （R1-1）：旧会话的 SessionEnd 在新会话的 SessionStart 之前。切回正在关闭的会话由内核的会话表保证：打开等它
    /// 关完再冷加载（session-lifecycle.md §3.3.1，F33；`leave` 在后台关的会话也一样）。`store`：会话里切过去时为真，
    /// 打开后让会话马上写下续写占位；启动形态的 `/resume` 页选中的会话为假，同 `--resume <id>`（§6.3.1，工作线 F79）。
    pub(super) fn switch(
        &mut self,
        id: SessionId,
        source: StartSource,
        notice: Option<String>,
        store: bool,
    ) {
        let store = store.then(|| self.envelope(Command::StoreResumeSentinel));
        let closing = self.detach();
        let kernel = self.kernel.clone();
        self.pending.push(
            async move {
                let ended = match closing {
                    Some(session) => session.close(EndReason::Resume).await,
                    None => Vec::new(),
                };
                let opened = open(kernel, id, source, None, store).await;
                Done::Switched(Box::new(opened.map(|(handle, view)| Switched {
                    handle,
                    view,
                    notice,
                    ended,
                })))
            }
            .boxed(),
        );
    }

    pub(super) fn on_switched(&mut self, switched: Result<Switched, KernelError>) {
        match switched {
            Ok(Switched {
                handle,
                view,
                notice,
                ended,
            }) => {
                self.app.reset();
                self.cache.clear();
                if let Some(info) = self.app.update_carry.take() {
                    self.app.local.push(crate::ui::app::LocalCell {
                        anchor: None,
                        local: crate::ui::app::Local::Update(Box::new(info)),
                    });
                }
                self.app.session = Some(handle.id().as_str().to_owned());
                let last = view.last_seq;
                self.app.view = Transcript::from_view(view, self.now_ms());
                self.app.sync_interaction(std::time::Instant::now());
                self.events = handle.subscribe(Some(last));
                self.session = Some(handle);
                // 切过去的会话可能已有后台任务与子代理（tui-display.md §1、tui-panels.md §1；`attach_panels` 取两者的
                // 同一份快照，修复线 F34）；footer 的请求统计从已有的用量行重建（F27，与新会话同一个查询）。
                self.refresh_stats();
                self.attach_panels();
                for ended in ended {
                    self.app.info(ended);
                }
                if let Some(notice) = notice {
                    self.app.info(notice);
                }
                self.after_switch(true);
            }
            Err(error) => {
                // 没有切换：提示还在草稿里。
                self.app.update_carry = None;
                self.after_switch(false);
                self.app.error(error.to_string());
            }
        }
    }

    /// rewind 选择器：取可选消息（带改动统计）与它们的文字。
    pub(super) fn open_rewind(&mut self) {
        let Some(session) = self.session.clone() else {
            self.app.open_rewind(Vec::new());
            return;
        };
        self.pending.push(
            async move {
                let entries = async {
                    let options = session.rewind_options().await?;
                    let view = session.view().await?;
                    Ok(options
                        .into_iter()
                        .map(|option| {
                            let text = view
                                .messages
                                .iter()
                                .find(|m| m.id() == &option.message)
                                .and_then(crate::ui::session_rows::user_text)
                                .unwrap_or_default();
                            Entry { option, text }
                        })
                        .collect())
                };
                Done::Rewinds(entries.await)
            }
            .boxed(),
        );
    }

    /// 选择器确认：运行中先中断，回合结束后再发（`deferred`，事件驱动，不等超时）。
    pub(super) fn rewind(&mut self, target: MessageId, scope: RewindScope, text: String) {
        let label = if scope == RewindScope::Code {
            String::new()
        } else {
            text
        };
        let command = Command::Rewind { target, scope };
        if self.app.running() {
            self.send(Sent::Interrupt, Command::Interrupt, String::new());
            self.deferred = Some((command, label));
            return;
        }
        self.send(Sent::Rewind, command, label);
    }

    /// 回合结束（视图回到空闲）时发出等待中的回退。
    pub(super) fn send_deferred(&mut self) {
        if self.app.running() {
            return;
        }
        if let Some((command, label)) = self.deferred.take() {
            self.send(Sent::Rewind, command, label);
        }
    }

    /// 入口收到退出信号（session-lifecycle.md §3.5）：中止在跑的回合，问 actor 回合是否还在跑——它在处理完
    /// 中止之后回复，回复是 running 时这个回合的 TurnFinished 一定在之后到达（`on_event` 据此离开），不用超时。
    /// 还没有会话时直接离开。中止用 `Shutdown`（修复线 F38）：TUI 收到任何退出信号都不中止 query、直接退出，
    /// 之后这个回合不再记录；以前按 esc 中断，空档里的输入与中断标记落库。
    pub(super) fn stop(&mut self) {
        self.stopping = true;
        let Some(session) = self.session.clone() else {
            self.app.quit = true;
            return;
        };
        // 命令 ID 经注入的 Ids（F7）。
        let shutdown = self.envelope(Command::Shutdown);
        self.pending.push(
            async move {
                let _ = session.send(shutdown).await;
                let running = session.view().await.is_ok_and(|view| view.running);
                Done::Stopping { running }
            }
            .boxed(),
        );
    }
}
