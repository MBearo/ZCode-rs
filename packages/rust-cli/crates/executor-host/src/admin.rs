//! 管理帧的客户端一侧（docs/specs/rust-cli/remote-exec.md §19.6）：连上常驻执行器的 socket，发一帧 `admin`，读一帧
//! `admin_reply`。代理（判断要不要拉起、换版本）与 `zcode executor status` / `stop` 用它。

use std::io;
use std::path::Path;
use std::time::Duration;

use rust_cli_exec_transport::MAX_FRAME;
use rust_cli_exec_transport::stream::stream_link;
use rust_cli_exec_transport::{FrameSink, FrameSource, Link};
use rust_cli_exec_wire::Body;
use rust_cli_exec_wire::admin::{
    AdminAction, AdminOutcome, AdminReply, AdminRequest, ExecutorStatus,
};
use rust_cli_exec_wire::frame::{Decoded, Frame, Kind, decode};

/// 等回复的上限：常驻执行器收到就回。超过时当作它卡住了（这是存活探测，不用来同步状态）。
const ANSWER: Duration = Duration::from_secs(10);

/// unix socket 路径的上限（`sun_path`，含结尾的 0）：Linux 108。
#[cfg(target_os = "linux")]
const SOCKET_PATH_MAX: usize = 108;
/// unix socket 路径的上限（`sun_path`，含结尾的 0）：macOS 104。
#[cfg(not(target_os = "linux"))]
const SOCKET_PATH_MAX: usize = 104;

/// socket 的路径超过系统上限时给出写明路径的错误（系统自己的错误看不出是哪个路径，§19.4）。
pub fn check_socket_path(socket: &Path) -> io::Result<()> {
    let length = socket.as_os_str().len();
    if length >= SOCKET_PATH_MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "the executor socket path {} is {length} bytes; this system allows at most {}",
                socket.display(),
                SOCKET_PATH_MAX - 1
            ),
        ));
    }
    Ok(())
}

/// 问一次。`Ok(None)`：连上了，但对方没有回 `admin_reply` 就断开（不认识管理帧的老执行器）。连不上（没有常驻进程、
/// 留下的 socket 文件）是 `Err`。
pub async fn request(socket: &Path, action: AdminAction) -> io::Result<Option<AdminReply>> {
    check_socket_path(socket)?;
    let halves = rust_cli_net::uds::connect_raw(socket).await?;
    let link = stream_link(halves.read, halves.write, "executor", MAX_FRAME);
    let (mut sink, mut source): (Box<dyn FrameSink>, Box<dyn FrameSource>) = Box::new(link).split();
    let frame = Frame::encode(Kind::Admin, &AdminRequest { action }, &Body::empty())
        .map_err(io::Error::other)?;
    sink.send(frame).await.map_err(io::Error::other)?;
    let answer = tokio::time::timeout(ANSWER, source.recv())
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "the executor did not answer"))?;
    let Ok(Some(bytes)) = answer else {
        return Ok(None);
    };
    match decode(bytes) {
        Ok(Decoded::Frame(frame)) if frame.kind == Kind::AdminReply => {
            frame.header().map(Some).map_err(io::Error::other)
        }
        _ => Ok(None),
    }
}

/// 常驻执行器的状态；没在跑时为 `None`。
pub async fn status(socket: &Path) -> io::Result<Option<ExecutorStatus>> {
    match request(socket, AdminAction::Status).await {
        Ok(reply) => Ok(reply.map(|reply| reply.status)),
        Err(error) if not_running(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

/// 连不上是因为没有常驻进程（没有 socket 文件，或文件是上次留下的）。
pub fn not_running(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    )
}

/// [`stop`] 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stopped {
    /// 本来就没在跑。
    NotRunning,
    /// 停了（进程已经退出）。
    Stopped(ExecutorStatus),
    /// `--if-idle`：不空闲，没有停。
    Busy(ExecutorStatus),
}

/// 让常驻执行器停下，等它退出（等它放开单实例锁：不轮询）。
pub async fn stop(socket: &Path, lock: &Path, if_idle: bool) -> io::Result<Stopped> {
    let action = if if_idle {
        AdminAction::StopIfIdle
    } else {
        AdminAction::Stop
    };
    let reply = match request(socket, action).await {
        Ok(Some(reply)) => reply,
        Ok(None) => {
            return Err(io::Error::other(
                "the running executor does not understand the stop request",
            ));
        }
        Err(error) if not_running(&error) => return Ok(Stopped::NotRunning),
        Err(error) => return Err(error),
    };
    match reply.outcome {
        AdminOutcome::Stopping => {
            wait_released(lock).await?;
            Ok(Stopped::Stopped(reply.status))
        }
        AdminOutcome::Busy => Ok(Stopped::Busy(reply.status)),
        AdminOutcome::Status | AdminOutcome::Other => {
            Err(io::Error::other("the running executor did not stop"))
        }
    }
}

/// 等单实例锁被放开（常驻进程退出时系统放开）。
pub async fn wait_released(lock: &Path) -> io::Result<()> {
    drop(rust_cli_fs::lock_exclusive(lock).await?);
    Ok(())
}
