//! 同进程的内存传输：一对双工的连接（有界，满了就等：背压），与一个“网络”（连接器每次连接得到一对，另一端交给
//! 监听方）。测试与同进程的 loopback 用它。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use tokio::sync::{Notify, mpsc};

use crate::link::{Connector, FrameSink, FrameSource, Link, LinkError, Listener};

/// 一个方向上缓冲的帧数。
const CAPACITY: usize = 64;

/// 内存连接的一端。
pub struct MemoryLink {
    tx: mpsc::Sender<Bytes>,
    rx: mpsc::Receiver<Bytes>,
    peer: String,
}

/// 一对连在一起的端。
pub fn pair(name: &str) -> (MemoryLink, MemoryLink) {
    let (left_tx, right_rx) = mpsc::channel(CAPACITY);
    let (right_tx, left_rx) = mpsc::channel(CAPACITY);
    (
        MemoryLink {
            tx: left_tx,
            rx: left_rx,
            peer: format!("{name}/server"),
        },
        MemoryLink {
            tx: right_tx,
            rx: right_rx,
            peer: format!("{name}/client"),
        },
    )
}

struct Sink(Option<mpsc::Sender<Bytes>>);

#[async_trait]
impl FrameSink for Sink {
    async fn send(&mut self, frame: Bytes) -> Result<(), LinkError> {
        let tx = self.0.as_ref().ok_or(LinkError::Closed)?;
        tx.send(frame).await.map_err(|_| LinkError::Closed)
    }

    async fn close(&mut self) -> Result<(), LinkError> {
        self.0 = None;
        Ok(())
    }
}

struct Source(mpsc::Receiver<Bytes>);

#[async_trait]
impl FrameSource for Source {
    async fn recv(&mut self) -> Result<Option<Bytes>, LinkError> {
        Ok(self.0.recv().await)
    }
}

impl Link for MemoryLink {
    fn split(self: Box<Self>) -> (Box<dyn FrameSink>, Box<dyn FrameSource>) {
        (Box::new(Sink(Some(self.tx))), Box::new(Source(self.rx)))
    }

    fn peer(&self) -> String {
        self.peer.clone()
    }
}

/// 一个内存“网络”：连接器的每次连接得到一对，服务端那一端交给监听方。
pub fn network(name: &str) -> (MemoryConnector, MemoryListener) {
    let hub = Arc::new(Hub {
        queue: Mutex::new(VecDeque::new()),
        notify: Notify::new(),
        listening: AtomicBool::new(true),
    });
    (
        MemoryConnector {
            name: name.to_owned(),
            hub: hub.clone(),
        },
        MemoryListener { hub },
    )
}

/// 等着被接受的连接（std 的锁只在取放时短暂持有，等待经 `Notify`）。
struct Hub {
    queue: Mutex<VecDeque<MemoryLink>>,
    notify: Notify,
    listening: AtomicBool,
}

pub struct MemoryConnector {
    name: String,
    hub: Arc<Hub>,
}

#[async_trait]
impl Connector for MemoryConnector {
    async fn connect(&self) -> Result<Box<dyn Link>, LinkError> {
        if !self.hub.listening.load(Ordering::SeqCst) {
            return Err(LinkError::Connect(format!(
                "{} is not listening",
                self.name
            )));
        }
        let (client, server) = pair(&self.name);
        if let Ok(mut queue) = self.hub.queue.lock() {
            queue.push_back(server);
        }
        self.hub.notify.notify_one();
        Ok(Box::new(client))
    }

    fn describe(&self) -> String {
        format!("memory:{}", self.name)
    }
}

pub struct MemoryListener {
    hub: Arc<Hub>,
}

impl MemoryListener {
    fn pop(&self) -> Option<MemoryLink> {
        self.hub
            .queue
            .lock()
            .ok()
            .and_then(|mut queue| queue.pop_front())
    }
}

impl Drop for MemoryListener {
    fn drop(&mut self) {
        self.hub.listening.store(false, Ordering::SeqCst);
    }
}

#[async_trait]
impl Listener for MemoryListener {
    async fn accept(&self) -> Result<Box<dyn Link>, LinkError> {
        loop {
            let notified = self.hub.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(link) = self.pop() {
                return Ok(Box::new(link));
            }
            notified.await;
        }
    }
}
