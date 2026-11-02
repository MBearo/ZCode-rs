//! 跟着一个会话的事件走（docs/specs/rust-cli/session-lifecycle.md §2.1）：会话日志的订阅流在订阅方落后（广播缓冲满了）
//! 时结束，订阅方按最后收到的 seq 重新订阅。三个前端（TUI、headless、App 协议的会话桥）都经 [`follow`]，规则只有一份。
//!
//! - 先建新流再丢旧流：旧流读到的位置一直拿着，会话日志不会去掉还没补发的（工作线 F120）。
//! - 连着 [`ENDED_AFTER_EMPTY_ROUNDS`] 轮新流一个事件都没给就结束：会话已经不在。落后之后的新流先补发落后期间的事件，
//!   不会是空的；留两轮是为了容下“新流还没给出事件就又落后”的一轮。

use std::sync::Arc;

use futures_util::stream::{self, BoxStream, StreamExt};

use crate::session_api::{SequencedEvent, SessionHandle};

/// 连续这么多轮重新订阅都没收到事件，就当会话的事件流结束了。
pub const ENDED_AFTER_EMPTY_ROUNDS: u32 = 2;

/// 跟随的状态：当前的订阅流、最后收到的 seq、这一轮有没有收到事件、连续的空轮数。
struct Follow {
    handle: Arc<dyn SessionHandle>,
    stream: BoxStream<'static, SequencedEvent>,
    last: u64,
    received: bool,
    empty: u32,
}

/// `after` 之后的事件，不重不漏；订阅流结束时从最后收到的 seq 重新订阅，会话不在了才结束。
pub fn follow(handle: Arc<dyn SessionHandle>, after: u64) -> BoxStream<'static, SequencedEvent> {
    let state = Follow {
        stream: handle.subscribe(Some(after)),
        handle,
        last: after,
        received: false,
        empty: 0,
    };
    stream::unfold(state, |mut state| async move {
        loop {
            if let Some(event) = state.stream.next().await {
                state.last = event.seq;
                state.received = true;
                return Some((event, state));
            }
            state.empty = if state.received { 0 } else { state.empty + 1 };
            state.received = false;
            if state.empty >= ENDED_AFTER_EMPTY_ROUNDS {
                return None;
            }
            // 先建新流，赋值时才丢旧流（旧流的位置在新流建好之前一直有效）。
            state.stream = state.handle.subscribe(Some(state.last));
        }
    })
    .boxed()
}

#[cfg(test)]
#[path = "session_follow_tests.rs"]
mod tests;
