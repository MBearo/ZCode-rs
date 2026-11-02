use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};

use super::*;
use crate::ids::SessionId;
use crate::session_api::{Ack, CommandEnvelope, KernelError, KernelEvent, SessionView};

fn title(seq: u64) -> SequencedEvent {
    SequencedEvent {
        seq,
        event: KernelEvent::TitleChanged {
            title: format!("t{seq}"),
        },
        revision: seq,
    }
}

/// 活着的订阅流的计数：流丢掉时减一。
struct Alive(Arc<AtomicUsize>);

impl Drop for Alive {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// 一个会话：订阅时给出 `after` 之后已有的事件；第一次订阅的流只给前两个（模拟落后之后流结束）。记下每次订阅的起点，
/// 与订阅时还活着的旧流个数。
struct Lagging {
    id: SessionId,
    events: Vec<SequencedEvent>,
    alive: Arc<AtomicUsize>,
    subscriptions: Mutex<Vec<(Option<u64>, usize)>>,
}

#[async_trait]
impl crate::testing::HandleOverride for Lagging {
    fn id(&self) -> &SessionId {
        &self.id
    }

    async fn send(&self, _command: CommandEnvelope) -> Ack {
        Ack::Duplicate
    }

    fn subscribe(&self, after: Option<u64>) -> BoxStream<'static, SequencedEvent> {
        let mut subscriptions = self.subscriptions.lock().unwrap();
        subscriptions.push((after, self.alive.load(Ordering::SeqCst)));
        let first = subscriptions.len() == 1;
        let events: Vec<_> = self
            .events
            .iter()
            .filter(|event| event.seq > after.unwrap_or(0))
            .take(if first { 2 } else { usize::MAX })
            .cloned()
            .collect();
        self.alive.fetch_add(1, Ordering::SeqCst);
        let guard = Alive(self.alive.clone());
        stream::iter(events)
            .map(move |event| {
                let _ = &guard;
                event
            })
            .boxed()
    }

    async fn view(&self) -> Result<SessionView, KernelError> {
        Err(KernelError::Unsupported {
            what: "view".to_owned(),
        })
    }
}

/// 第一个流给了 2、3 就结束（落后）；第二个从 3 之后接；之后两次空流就结束。每次重新订阅时旧流还活着。
#[tokio::test]
async fn a_lagging_stream_is_resumed_from_the_last_seq_until_the_session_is_gone() {
    let handle = Arc::new(Lagging {
        id: SessionId::new("s1"),
        events: (1..=5).map(title).collect(),
        alive: Arc::new(AtomicUsize::new(0)),
        subscriptions: Mutex::new(Vec::new()),
    });
    let seqs: Vec<u64> = follow(handle.clone(), 1)
        .map(|event| event.seq)
        .collect()
        .await;
    assert_eq!(seqs, vec![2, 3, 4, 5]);
    assert_eq!(
        *handle.subscriptions.lock().unwrap(),
        vec![(Some(1), 0), (Some(3), 1), (Some(5), 1), (Some(5), 1)]
    );
    assert_eq!(handle.alive.load(Ordering::SeqCst), 0);
}
