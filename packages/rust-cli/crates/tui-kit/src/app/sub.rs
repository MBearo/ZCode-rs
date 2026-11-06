//! 对事件源的声明式订阅，每次 `update` 之后从状态重新推导。
//! 移植自 packages/tui/tui/src/zcode/tui/app/Sub.scala（feat/tui a2bf878b34）：回调式的 `OnSource` 变成按 key 标识的
//! 流（`Source`）：运行时在一个 key 第一次出现时打开流，消失时丢掉它；key 还在时流照旧（没有 Scala 那种可换的 `toMsg`：消息在流里已经映射好）。
use std::sync::Arc;

use futures_util::stream::BoxStream;

use super::key::Key;
use super::screen::Viewport;

/// 一个按键处理器：带着下一帧的视口（在折行的输入里 ↑ 的意思取决于布局，读的宽度与 `view` 相同）。
pub type KeyHandler<M> = Arc<dyn Fn(&Key, Viewport) -> Option<M> + Send + Sync>;

/// 打开一个推送式的源：返回它的流。
pub type Open<M> = Arc<dyn Fn() -> BoxStream<'static, M> + Send + Sync>;

pub enum Sub<M> {
    OnKey(KeyHandler<M>),
    /// 每 `ms` 毫秒一条 `msg`。身份是 `(ms, msg)`：消息必须可比较、可克隆。
    Every {
        ms: u64,
        msg: M,
    },
    /// 推送式的源。`key` 是它跨推导的身份（函数值没有有用的相等性，没有 key 每次 update 都会重订阅，订阅时重放快照的源
    /// 就会循环）；key 消失后到达的值被丢掉。
    Source {
        key: String,
        open: Open<M>,
    },
}

impl<M> Sub<M> {
    pub fn on_key(handler: impl Fn(&Key, Viewport) -> Option<M> + Send + Sync + 'static) -> Sub<M> {
        Sub::OnKey(Arc::new(handler))
    }

    pub fn on_key_press(handler: impl Fn(&Key) -> Option<M> + Send + Sync + 'static) -> Sub<M> {
        Sub::OnKey(Arc::new(move |k, _| handler(k)))
    }

    pub fn every(ms: u64, msg: M) -> Sub<M> {
        Sub::Every { ms, msg }
    }

    pub fn source(
        key: impl Into<String>,
        open: impl Fn() -> BoxStream<'static, M> + Send + Sync + 'static,
    ) -> Sub<M> {
        Sub::Source {
            key: key.into(),
            open: Arc::new(open),
        }
    }
}

impl<M: std::fmt::Debug> std::fmt::Debug for Sub<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Sub::OnKey(_) => f.write_str("OnKey(..)"),
            Sub::Every { ms, msg } => f
                .debug_struct("Every")
                .field("ms", ms)
                .field("msg", msg)
                .finish(),
            Sub::Source { key, .. } => f
                .debug_struct("Source")
                .field("key", key)
                .finish_non_exhaustive(),
        }
    }
}
