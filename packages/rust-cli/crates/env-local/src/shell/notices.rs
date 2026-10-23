//! 后台命令终态通知的流（README §6 规则 6：至少一次送达）。
//!
//! 先送达 `seq > after` 且没有 ack 的通知，再等新的。通知在 ack 之前一直留在会话里，重新订阅
//! 时会再次送达；接收方按 seq 去重。

use std::sync::Arc;

use futures_util::stream::{self, BoxStream, StreamExt};
use rust_cli_kernel_api::env::EnvError;
use rust_cli_kernel_api::env_shell::ShellNotice;
use rust_cli_kernel_api::ids::SessionId;

use super::Shared;

struct Cursor {
    shared: Arc<Shared>,
    session: SessionId,
    after: u64,
}

async fn next(mut cursor: Cursor) -> Option<(Result<ShellNotice, EnvError>, Cursor)> {
    let mut changed = cursor.shared.notices.subscribe();
    loop {
        changed.borrow_and_update();
        let pending = {
            let state = cursor.shared.state();
            state.sessions.get(&cursor.session).and_then(|shell| {
                shell
                    .notices
                    .iter()
                    .find(|notice| notice.seq > cursor.after)
                    .cloned()
            })
        };
        if let Some(notice) = pending {
            cursor.after = notice.seq;
            return Some((Ok(notice), cursor));
        }
        if changed.changed().await.is_err() {
            return None;
        }
    }
}

pub(crate) fn stream(
    shared: Arc<Shared>,
    session: SessionId,
    after: u64,
) -> BoxStream<'static, Result<ShellNotice, EnvError>> {
    stream::unfold(
        Cursor {
            shared,
            session,
            after,
        },
        next,
    )
    .boxed()
}
