//! 字节流上的帧（docs/specs/rust-cli/remote-exec.md §18.2）：每帧前面是 u32 BE 的长度。stdio 与以后的 unix socket
//! 共用。

use async_trait::async_trait;
use bytes::Bytes;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::link::{FrameSink, FrameSource, Link, LinkError};

/// 把一对字节流包成一条连接。`max_frame` 是单帧的上限（收发两个方向都查）。
pub fn stream_link<R, W>(
    reader: R,
    writer: W,
    peer: impl Into<String>,
    max_frame: usize,
) -> StreamLink
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    StreamLink {
        sink: Box::new(StreamSink {
            writer: Some(Box::new(writer)),
            max_frame,
        }),
        source: Box::new(StreamSource {
            reader: Box::new(reader),
            max_frame,
        }),
        peer: peer.into(),
    }
}

pub struct StreamLink {
    pub(crate) sink: Box<dyn FrameSink>,
    pub(crate) source: Box<dyn FrameSource>,
    pub(crate) peer: String,
}

impl Link for StreamLink {
    fn split(self: Box<Self>) -> (Box<dyn FrameSink>, Box<dyn FrameSource>) {
        (self.sink, self.source)
    }

    fn peer(&self) -> String {
        self.peer.clone()
    }
}

fn io(error: &std::io::Error) -> LinkError {
    match error.kind() {
        std::io::ErrorKind::BrokenPipe
        | std::io::ErrorKind::ConnectionReset
        | std::io::ErrorKind::UnexpectedEof => LinkError::Closed,
        _ => LinkError::Io(error.to_string()),
    }
}

struct StreamSink {
    writer: Option<Box<dyn AsyncWrite + Unpin + Send>>,
    max_frame: usize,
}

#[async_trait]
impl FrameSink for StreamSink {
    async fn send(&mut self, frame: Bytes) -> Result<(), LinkError> {
        if frame.len() > self.max_frame {
            return Err(LinkError::TooLarge {
                size: frame.len(),
                max: self.max_frame,
            });
        }
        let writer = self.writer.as_mut().ok_or(LinkError::Closed)?;
        let len = u32::try_from(frame.len()).map_err(|_| LinkError::TooLarge {
            size: frame.len(),
            max: self.max_frame,
        })?;
        writer
            .write_all(&len.to_be_bytes())
            .await
            .map_err(|e| io(&e))?;
        writer.write_all(&frame).await.map_err(|e| io(&e))?;
        writer.flush().await.map_err(|e| io(&e))
    }

    async fn close(&mut self) -> Result<(), LinkError> {
        if let Some(mut writer) = self.writer.take() {
            writer.shutdown().await.map_err(|e| io(&e))?;
        }
        Ok(())
    }
}

struct StreamSource {
    reader: Box<dyn AsyncRead + Unpin + Send>,
    max_frame: usize,
}

#[async_trait]
impl FrameSource for StreamSource {
    async fn recv(&mut self) -> Result<Option<Bytes>, LinkError> {
        let mut len = [0u8; 4];
        // 在帧的边界上结束是正常关闭；帧读到一半结束是错误。
        let mut read = 0;
        while read < len.len() {
            let n = self
                .reader
                .read(&mut len[read..])
                .await
                .map_err(|e| io(&e))?;
            if n == 0 {
                return if read == 0 {
                    Ok(None)
                } else {
                    Err(LinkError::Io(
                        "stream ended inside a frame length".to_owned(),
                    ))
                };
            }
            read += n;
        }
        let size = u32::from_be_bytes(len) as usize;
        if size > self.max_frame {
            return Err(LinkError::TooLarge {
                size,
                max: self.max_frame,
            });
        }
        let mut frame = vec![0u8; size];
        self.reader
            .read_exact(&mut frame)
            .await
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::UnexpectedEof => {
                    LinkError::Io("stream ended inside a frame".to_owned())
                }
                _ => io(&error),
            })?;
        Ok(Some(Bytes::from(frame)))
    }
}
