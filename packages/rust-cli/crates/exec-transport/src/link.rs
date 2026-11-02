//! 传输的端口（docs/specs/rust-cli/remote-exec.md §15.3）。帧是一段不透明的字节。

use async_trait::async_trait;
use bytes::Bytes;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LinkError {
    /// 连接已断开（对方关闭、进程退出或被故障注入切断）。
    #[error("link closed")]
    Closed,
    #[error("link io error: {0}")]
    Io(String),
    #[error("frame of {size} bytes exceeds the {max}-byte limit")]
    TooLarge { size: usize, max: usize },
    /// 连不上（起不了进程、对方不在）。
    #[error("cannot connect: {0}")]
    Connect(String),
    /// 目标拒绝了这次连接，再连也一样（ssh 认证失败、主机密钥不对、远端的执行器程序不在了）：由知道传输的连接器判断
    /// （ssh-connect），客户端看到它就不再重连（远端执行 F96，remote-exec.md §23.4）。内容是原因原文。
    #[error("{0}")]
    Rejected(String),
}

/// 一条连接的发送方向。
#[async_trait]
pub trait FrameSink: Send {
    /// 发一帧；对方已断开时是 [`LinkError::Closed`]。
    async fn send(&mut self, frame: Bytes) -> Result<(), LinkError>;

    /// 关闭发送方向：对方之后收到“结束”。
    async fn close(&mut self) -> Result<(), LinkError>;
}

/// 一条连接的接收方向。
#[async_trait]
pub trait FrameSource: Send {
    /// 收一帧；对方正常关闭时是 `Ok(None)`。
    async fn recv(&mut self) -> Result<Option<Bytes>, LinkError>;
}

/// 一条连接：分成收发两端，各自交给一个任务。
pub trait Link: Send {
    fn split(self: Box<Self>) -> (Box<dyn FrameSink>, Box<dyn FrameSource>);

    /// 对方是谁（日志与错误文字）。
    fn peer(&self) -> String;
}

/// 执行器侧：接受连接。
#[async_trait]
pub trait Listener: Send + Sync {
    /// 下一条连接；监听已经关闭时是 [`LinkError::Closed`]。
    async fn accept(&self) -> Result<Box<dyn Link>, LinkError>;
}

/// 控制面侧：到达一个目标，得到一条新的连接（第一次连接与重连都经它）。
#[async_trait]
pub trait Connector: Send + Sync {
    async fn connect(&self) -> Result<Box<dyn Link>, LinkError>;

    /// 目标是谁（日志与错误文字）。
    fn describe(&self) -> String;
}
