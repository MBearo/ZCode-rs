//! 故障注入（docs/specs/rust-cli/remote-exec.md §18.7）：包住一条连接，在发送方向丢帧、延迟、扣住一帧放到下一帧之后
//! （乱序），或者整条切断（两个方向都结束）。传输不认识帧的内容，所以“哪一帧”由测试给的条件判断。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use tokio::sync::Notify;

use crate::link::{FrameSink, FrameSource, Link, LinkError};

/// 判断一帧（发送前的字节）。
pub type Matcher = Arc<dyn Fn(&[u8]) -> bool + Send + Sync>;

/// 注入哪些故障（都作用在被包住的这一端的发送方向）。
#[derive(Clone, Default)]
pub struct Faults {
    drop: Option<(Matcher, usize)>,
    hold: Option<Matcher>,
    delay: Option<Duration>,
    cut_after_sent: Option<usize>,
}

impl Faults {
    pub fn new() -> Self {
        Self::default()
    }

    /// 丢掉满足条件的帧，最多 `times` 次（对方收不到；连接照常）。
    #[must_use]
    pub fn drop_sent(mut self, matcher: Matcher, times: usize) -> Self {
        self.drop = Some((matcher, times));
        self
    }

    /// 扣住满足条件的一帧，等这一端再发一帧之后再放出：两帧的顺序颠倒。
    #[must_use]
    pub fn hold_sent(mut self, matcher: Matcher) -> Self {
        self.hold = Some(matcher);
        self
    }

    /// 每帧发出前等一会。
    #[must_use]
    pub fn delay_sent(mut self, delay: Duration) -> Self {
        self.delay = Some(delay);
        self
    }

    /// 发出第 `count` 帧之后切断（这一帧之后的帧都丢掉）。
    #[must_use]
    pub fn cut_after_sent(mut self, count: usize) -> Self {
        self.cut_after_sent = Some(count);
        self
    }
}

/// 测试手里的开关：随时切断这条连接（两个方向都结束，对方读到结束、发送失败）。
#[derive(Clone, Default)]
pub struct Switch {
    cut: Arc<AtomicBool>,
    notify: Arc<Notify>,
    /// 被包住的发送端：切断时丢掉它，对方随即读到结束（发送端可能正闲着，不能等下一次发送）。
    sink: Arc<Mutex<Option<Box<dyn FrameSink>>>>,
}

impl Switch {
    pub fn cut(&self) {
        self.cut.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
        let sink = self.sink.lock().ok().and_then(|mut sink| sink.take());
        drop(sink);
    }

    pub fn is_cut(&self) -> bool {
        self.cut.load(Ordering::SeqCst)
    }
}

/// 包住一条连接；返回包好的连接与它的开关。
pub fn inject(link: Box<dyn Link>, faults: Faults) -> (Box<dyn Link>, Switch) {
    let switch = Switch::default();
    (
        Box::new(Faulty {
            inner: link,
            faults,
            switch: switch.clone(),
        }),
        switch,
    )
}

struct Faulty {
    inner: Box<dyn Link>,
    faults: Faults,
    switch: Switch,
}

impl Link for Faulty {
    fn split(self: Box<Self>) -> (Box<dyn FrameSink>, Box<dyn FrameSource>) {
        let peer = self.inner.peer();
        let (sink, source) = self.inner.split();
        if let Ok(mut slot) = self.switch.sink.lock() {
            *slot = Some(sink);
        }
        let drops = self.faults.drop.as_ref().map_or(0, |(_, times)| *times);
        (
            Box::new(FaultySink {
                faults: self.faults,
                switch: self.switch.clone(),
                sent: 0,
                drops_left: drops,
                held: None,
                peer,
            }),
            Box::new(FaultySource {
                inner: Some(source),
                switch: self.switch,
            }),
        )
    }

    fn peer(&self) -> String {
        self.inner.peer()
    }
}

struct FaultySink {
    faults: Faults,
    switch: Switch,
    sent: usize,
    drops_left: usize,
    held: Option<Bytes>,
    peer: String,
}

impl FaultySink {
    /// 经被包住的发送端发一帧：取出来发，发完放回；发的时候被切断了就不放回（丢掉）。
    async fn forward(&self, frame: Bytes) -> Result<(), LinkError> {
        let taken = self
            .switch
            .sink
            .lock()
            .ok()
            .and_then(|mut sink| sink.take());
        let Some(mut sink) = taken else {
            return Err(LinkError::Closed);
        };
        let sent = sink.send(frame).await;
        if !self.switch.is_cut()
            && let Ok(mut slot) = self.switch.sink.lock()
        {
            *slot = Some(sink);
        }
        sent
    }
}

#[async_trait]
impl FrameSink for FaultySink {
    async fn send(&mut self, frame: Bytes) -> Result<(), LinkError> {
        if self.switch.is_cut() {
            return Err(LinkError::Closed);
        }
        if let Some(delay) = self.faults.delay {
            tokio::time::sleep(delay).await;
        }
        if let Some((matcher, _)) = &self.faults.drop
            && self.drops_left > 0
            && matcher(&frame)
        {
            self.drops_left -= 1;
            return Ok(());
        }
        if let Some(matcher) = &self.faults.hold
            && self.held.is_none()
            && matcher(&frame)
        {
            self.held = Some(frame);
            return Ok(());
        }
        self.forward(frame).await?;
        if let Some(held) = self.held.take() {
            self.faults.hold = None;
            self.forward(held).await?;
        }
        self.sent += 1;
        if self
            .faults
            .cut_after_sent
            .is_some_and(|count| self.sent >= count)
        {
            tracing::debug!(peer = %self.peer, "fault injection cut the link");
            self.switch.cut();
        }
        Ok(())
    }

    async fn close(&mut self) -> Result<(), LinkError> {
        let taken = self
            .switch
            .sink
            .lock()
            .ok()
            .and_then(|mut sink| sink.take());
        match taken {
            Some(mut sink) => sink.close().await,
            None => Ok(()),
        }
    }
}

struct FaultySource {
    inner: Option<Box<dyn FrameSource>>,
    switch: Switch,
}

#[async_trait]
impl FrameSource for FaultySource {
    async fn recv(&mut self) -> Result<Option<Bytes>, LinkError> {
        let notified = self.switch.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let inner = match &mut self.inner {
            Some(inner) if !self.switch.is_cut() => inner,
            _ => {
                // 切断之后丢掉接收端：对方再发就失败。
                self.inner = None;
                return Err(LinkError::Closed);
            }
        };
        tokio::select! {
            received = inner.recv() => received,
            () = &mut notified => {
                self.inner = None;
                Err(LinkError::Closed)
            }
        }
    }
}

#[cfg(test)]
#[path = "fault_tests.rs"]
mod tests;
