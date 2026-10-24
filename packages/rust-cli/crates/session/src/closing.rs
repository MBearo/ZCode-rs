//! 关闭会话（docs/specs/rust-cli/session-lifecycle.md §3.3、§3.3.1）：只关一次，关闭任务在内核的根作用域里跑完。
//!
//! 修复（F33）：以前关闭在调用方的任务里跑，第二次 `close` 在 actor 还在时再跑一次 SessionEnd（并让第一次的
//! `Request::Close` 失去回复）；调用方丢下 future 时会话停在半关的状态。会话表加了“关闭中”之后，打开同一个会话要
//! 等关闭走完，所以关闭必须不依赖调用方：第一次 `close` 起关闭任务，之后的调用等同一个任务结束。

use std::sync::PoisonError;

use rust_cli_kernel_api::abort::AbortReason;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::session_api::{EndReason, KernelError};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::{CancellationToken, DropGuard};

use crate::actor::Request;
use crate::{Session, SessionDeps, hooks};

/// 发一个请求给 actor、等回复；actor 已经退出时为 `closed`。
pub(crate) async fn ask<T>(
    requests: &mpsc::Sender<Request>,
    request: impl FnOnce(oneshot::Sender<T>) -> Request,
) -> Result<T, KernelError> {
    let (reply, answer) = oneshot::channel();
    requests
        .send(request(reply))
        .await
        .map_err(|_| crate::closed())?;
    answer.await.map_err(|_| crate::closed())
}

/// `SessionHandle::close`：第一次起关闭任务并等它的结果；关闭中又来关闭（或已经关完）时等同一个任务结束，返回
/// 空的提示——SessionEnd 只跑一次，提示归第一次的调用方。
pub(crate) async fn close(session: &Session, reason: EndReason) -> Vec<String> {
    // 关闭的进度（`Session::closing`）：None 还没关；Some 关闭任务已经起了，令牌在任务结束（或被丢下）时取消。
    let first = {
        let mut latch = session
            .closing
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match latch.as_ref() {
            Some(running) => Err(running.clone()),
            None => {
                let done = CancellationToken::new();
                *latch = Some(done.clone());
                Ok(done)
            }
        }
    };
    let done = match first {
        Ok(done) => done,
        Err(running) => {
            running.cancelled().await;
            return Vec::new();
        }
    };
    // 会话表：Open → Closing，早于 SessionEnd，并且就在这一次轮询里（不等关闭任务被调度）：从这时起打开同一个会话
    // 都等它关完（§3.3.1）。
    session.deps.released.closing(&session.id);
    let work = run(
        session.id.clone(),
        session.requests.clone(),
        session.deps.clone(),
        reason,
        // 守卫随关闭任务的 future 一起丢弃：任务结束、被中止、一次都没跑，等它的 `close` 都会醒来。
        done.drop_guard(),
    );
    let spawner = &session.deps.closing;
    if spawner.is_closed() {
        // 内核正在释放（根作用域已关闭）：在调用方的任务里跑完。
        return work.await;
    }
    spawner
        .spawn("session-close", work)
        .await
        .unwrap_or_default()
}

/// 关闭任务（spec §3.3 的关闭路径）。每一步失败都照常往下走：最后的 `released` 总会把会话移出会话表。
async fn run(
    id: SessionId,
    requests: mpsc::Sender<Request>,
    deps: SessionDeps,
    reason: EndReason,
    _done: DropGuard,
) -> Vec<String> {
    tracing::info!(event = "session.close", session = %id, reason = ?reason, "session closed");
    let notices = match ask(&requests, |reply| Request::Meta { reply }).await {
        Ok(meta) => hooks::session_end(&deps, &meta, reason).await,
        // actor 已经没了（例如 panic）：没有 meta，不跑 SessionEnd，照常释放。
        Err(_) => Vec::new(),
    };
    // actor 已经没了时这一步失败；同样照常往下走。退出（`prompt_input_exit`、`other`）不再记录在跑的回合
    // （`Shutdown`，修复线 F38，spec §3.5）：`/exit` 与退出信号不中止 query、直接退出。
    let abort = match reason {
        EndReason::Clear | EndReason::Resume => AbortReason::User,
        EndReason::Logout | EndReason::PromptInputExit | EndReason::Other => AbortReason::Shutdown,
    };
    let _ = ask(&requests, |reply| Request::Close {
        reply,
        reason: abort,
    })
    .await;
    // 后台任务跨 `/clear`、`/resume` 保留（只停前台任务）：这两种关闭
    // 不让执行环境忘掉会话（后台命令继续跑、通知留着）；退出程序时全部释放。
    let forget = !matches!(reason, EndReason::Clear | EndReason::Resume);
    if let Err(error) = deps.machine.commands().close_session(&id, forget).await {
        tracing::warn!(%error, session = %id, "the environment could not close the session");
    }
    // 连上的远端机器同样关闭（R3，remote-exec.md §20.7.2）：没连上的那台以后重连时不会再送这个会话的通知给它。
    for remote in deps.machines.machines().machines {
        let Some(machine) = deps.machines.machine(&remote.name) else {
            continue;
        };
        if let Err(error) = machine.commands().close_session(&id, forget).await {
            tracing::warn!(%error, session = %id, machine = %remote.name, "a remote machine could not close the session");
        }
    }
    // 停止这个会话的持久定时任务调度、释放调度锁（tools-more.md §16.4）。
    deps.cron.session_closed(&id).await;
    // 最后一步：组装方清按会话的表、把会话移出会话表（等它的打开在这之后醒来）。
    deps.released.released(&id);
    notices
}
