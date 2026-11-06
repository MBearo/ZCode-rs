//! 订阅的对账：每次 `update` 之后从状态推导的订阅集与正在运行的源对齐。
//! 移植自 packages/tui/tui/src/zcode/tui/app/Runtime.scala 的 `reconcile*`（feat/tui a2bf878b34）。
//!
//! - 按键处理器按顺序折叠：第一个给出消息的胜出。
//! - 定时器的身份是 `(ms, msg)`：还在的不动，消失的中止，新出现的启动。定时器的任务只送自己的 id；循环按 id 找消息，
//!   已经移除的定时器排在队列里的 tick 也不会再折叠。
//! - 源的身份是 key：key 第一次出现时打开流，消失时丢掉流（就是退订）。流由循环自己轮询（不经转发任务），所以一个源
//!   在打开时或回调里同步放进去的值，在循环下一次收集时就已经到了——握手落定的报告排在它的重画之前靠的就是这个。
//!   投递带着源的 id：同一批里先处理的事件丢掉了这个 key 时，它已经收集到的值被丢掉。
use std::task::{Context, Poll};
use std::time::Duration;

use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use rust_cli_base::{Spawner, Task};
use tokio::sync::mpsc::UnboundedSender;

use super::Event;
use crate::app::key::Key;
use crate::app::screen::Viewport;
use crate::app::sub::{KeyHandler, Sub};

/// 一次收集从一个源最多取这么多个值：永远就绪的流不能让循环停在收集里。
const PER_SOURCE: usize = 1024;

struct Timer<M> {
    ms: u64,
    msg: M,
    id: u64,
    task: Task<Option<()>>,
}

struct Live<M> {
    key: String,
    id: u64,
    /// 流结束了是 `None`：key 还留着，下次推导不会重新打开它。
    stream: Option<BoxStream<'static, M>>,
}

/// 运行中的订阅。只由循环任务持有。
pub(super) struct Subs<M> {
    keys: Vec<KeyHandler<M>>,
    timers: Vec<Timer<M>>,
    sources: Vec<Live<M>>,
    next_id: u64,
}

impl<M: Clone + PartialEq + Send + 'static> Subs<M> {
    pub(super) fn new() -> Subs<M> {
        Subs {
            keys: Vec::new(),
            timers: Vec::new(),
            sources: Vec::new(),
            next_id: 0,
        }
    }

    fn fresh_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// 与新推导的订阅集对齐。
    pub(super) fn reconcile(
        &mut self,
        wanted: Vec<Sub<M>>,
        spawner: &Spawner,
        tx: &UnboundedSender<Event<M>>,
    ) {
        let mut keys = Vec::new();
        let mut timers: Vec<(u64, M)> = Vec::new();
        let mut sources = Vec::new();
        for sub in wanted {
            match sub {
                Sub::OnKey(h) => keys.push(h),
                Sub::Every { ms, msg } => {
                    if !timers.iter().any(|(m, x)| *m == ms && *x == msg) {
                        timers.push((ms, msg));
                    }
                }
                // 同一个 key 出现两次时第一个算数（Scala 的 Map 也只留一个）。
                Sub::Source { key, open } => {
                    if !sources.iter().any(|(k, _)| *k == key) {
                        sources.push((key, open));
                    }
                }
            }
        }
        self.keys = keys;

        self.timers.retain(|t| {
            let keep = timers.iter().any(|(ms, msg)| t.ms == *ms && t.msg == *msg);
            if !keep {
                t.task.abort();
            }
            keep
        });
        for (ms, msg) in timers {
            if self.timers.iter().any(|t| t.ms == ms && t.msg == msg) {
                continue;
            }
            let id = self.fresh_id();
            let task = spawn_ticker(spawner, tx.clone(), ms, id);
            self.timers.push(Timer { ms, msg, id, task });
        }

        // 丢掉流就是退订。
        self.sources
            .retain(|live| sources.iter().any(|(key, _)| *key == live.key));
        for (key, open) in sources {
            if self.sources.iter().any(|live| live.key == key) {
                continue;
            }
            let id = self.fresh_id();
            // 打开时同步送出的值（快照的重放）留在流里，下一次收集时取到。
            let stream = open();
            self.sources.push(Live {
                key,
                id,
                stream: Some(stream),
            });
        }
    }

    /// 按顺序问每个按键处理器，第一个给出的消息。
    pub(super) fn key(&self, key: &Key, viewport: Viewport) -> Option<M> {
        self.keys.iter().find_map(|h| h(key, viewport))
    }

    /// 定时器 `id` 的消息；它已经移除时是 `None`。
    pub(super) fn tick(&self, id: u64) -> Option<M> {
        self.timers
            .iter()
            .find(|t| t.id == id)
            .map(|t| t.msg.clone())
    }

    /// 源 `id` 还订阅着。
    pub(super) fn is_live(&self, id: u64) -> bool {
        self.sources.iter().any(|live| live.id == id)
    }

    /// 收集各个源已经就绪的值，按源的订阅顺序。
    pub(super) fn collect(&mut self, cx: &mut Context<'_>, out: &mut Vec<Event<M>>) {
        for live in &mut self.sources {
            let Some(stream) = live.stream.as_mut() else {
                continue;
            };
            for _ in 0..PER_SOURCE {
                match stream.poll_next_unpin(cx) {
                    Poll::Ready(Some(msg)) => out.push(Event::Delivery {
                        source: live.id,
                        msg,
                    }),
                    Poll::Ready(None) => {
                        live.stream = None;
                        break;
                    }
                    Poll::Pending => break,
                }
            }
        }
    }

    /// 收尾：退订全部源、停掉全部定时器。
    pub(super) fn clear(&mut self) {
        self.keys.clear();
        for t in self.timers.drain(..) {
            t.task.abort();
        }
        self.sources.clear();
    }
}

/// 每 `ms` 毫秒送一次 `Event::Tick(id)`。
fn spawn_ticker<M: Send + 'static>(
    spawner: &Spawner,
    tx: UnboundedSender<Event<M>>,
    ms: u64,
    id: u64,
) -> Task<Option<()>> {
    // 0 毫秒的定时器会让任务空转：至少 1 毫秒。
    let period = Duration::from_millis(ms.max(1));
    spawner.spawn_resident("puffin-runtime: every", async move {
        loop {
            tokio::time::sleep(period).await;
            if tx.send(Event::Tick(id)).is_err() {
                break;
            }
        }
    })
}
