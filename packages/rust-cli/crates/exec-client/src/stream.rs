//! 客户端这一头的流：按 `seq` 交给消费方；消费方每取走一项就记下 ack（写任务空闲时合成一帧发出）；丢掉流时发
//! `close`。

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures_util::Stream;
use tokio::sync::mpsc;

use crate::client::abandon;
use crate::state::{Delivered, Shared};
use crate::{ClientError, StreamEvent};

pub struct ClientStream {
    rx: mpsc::Receiver<Delivered>,
    call: u64,
    shared: Arc<Shared>,
    done: bool,
}

impl ClientStream {
    pub(crate) fn new(rx: mpsc::Receiver<Delivered>, call: u64, shared: Arc<Shared>) -> Self {
        Self {
            rx,
            call,
            shared,
            done: false,
        }
    }

    /// 这个流的调用 ID。
    pub fn call(&self) -> u64 {
        self.call
    }
}

impl Stream for ClientStream {
    type Item = Result<StreamEvent, ClientError>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.done {
            return Poll::Ready(None);
        }
        match self.rx.poll_recv(context) {
            Poll::Ready(Some(Ok((seq, event)))) => {
                let call = self.call;
                self.shared.state().ack_stream(call, seq);
                self.shared.ack.notify_one();
                Poll::Ready(Some(Ok(event)))
            }
            Poll::Ready(Some(Err(error))) => {
                self.done = true;
                Poll::Ready(Some(Err(error)))
            }
            Poll::Ready(None) => {
                self.done = true;
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for ClientStream {
    fn drop(&mut self) {
        if !self.done {
            abandon(&self.shared, self.call);
        }
    }
}
