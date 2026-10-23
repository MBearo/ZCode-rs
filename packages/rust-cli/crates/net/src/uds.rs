//! 本机的 Unix 域套接字（跨会话的收件箱，docs/specs/rust-cli/cross-session.md §2.1）。
//!
//! 只做传输：监听、接受连接、按行读写（NDJSON），连接后写一行、读一行回复。帧的含义、登记与路由在适配器
//! `rust-cli-peers`。socket 文件的权限由调用方经 `rust-cli-fs` 收紧（所在目录本来就是 0700）。
//! Windows 没有 tokio 的 Unix 域套接字：全部返回 `Unsupported`，跨会话在那里关闭（§5 D9）。

use std::io;
use std::path::Path;

/// 一行最多这么多字节（超过时断开连接）。
pub const MAX_LINE: usize = 1 << 20;

#[cfg(unix)]
mod imp {
    use std::io;
    use std::path::{Path, PathBuf};

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

    use super::MAX_LINE;

    /// 一个监听中的收件箱。
    pub struct UdsListener {
        inner: tokio::net::UnixListener,
        path: PathBuf,
    }

    impl UdsListener {
        /// 在 `path` 上监听。路径上已有文件时报 `AddrInUse`（调用方先判断它是不是失效的 socket）。
        #[expect(
            clippy::disallowed_methods,
            reason = "net 是套接字的唯一入口（README §4）：跨会话的收件箱在这里监听"
        )]
        pub fn bind(path: &Path) -> io::Result<Self> {
            let inner = tokio::net::UnixListener::bind(path)?;
            Ok(Self {
                inner,
                path: path.to_path_buf(),
            })
        }

        pub fn path(&self) -> &Path {
            &self.path
        }

        pub async fn accept(&self) -> io::Result<UdsStream> {
            let (stream, _) = self.inner.accept().await?;
            Ok(UdsStream::new(stream))
        }
    }

    /// 一个连接：按行读写。
    pub struct UdsStream {
        reader: BufReader<OwnedReadHalf>,
        writer: OwnedWriteHalf,
    }

    impl UdsStream {
        fn new(stream: tokio::net::UnixStream) -> Self {
            let (read, write) = stream.into_split();
            Self {
                reader: BufReader::new(read),
                writer: write,
            }
        }

        /// 下一行（不含换行）；对方关闭时为 None。超过 [`MAX_LINE`] 时报 `InvalidData`。
        pub async fn read_line(&mut self) -> io::Result<Option<String>> {
            let mut bytes = Vec::new();
            loop {
                let available = self.reader.fill_buf().await?;
                if available.is_empty() {
                    return Ok((!bytes.is_empty()).then(|| lossy(bytes)));
                }
                if let Some(at) = available.iter().position(|byte| *byte == b'\n') {
                    bytes.extend_from_slice(&available[..at]);
                    self.reader.consume(at + 1);
                    return Ok(Some(lossy(bytes)));
                }
                let taken = available.len();
                bytes.extend_from_slice(available);
                self.reader.consume(taken);
                if bytes.len() > MAX_LINE {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "line exceeds the limit",
                    ));
                }
            }
        }

        /// 写一行（自动补换行）并刷出。
        pub async fn write_line(&mut self, line: &str) -> io::Result<()> {
            self.writer.write_all(line.as_bytes()).await?;
            self.writer.write_all(b"\n").await?;
            self.writer.flush().await
        }
    }

    fn lossy(bytes: Vec<u8>) -> String {
        String::from_utf8(bytes)
            .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned())
    }

    /// 连接 `path`。
    #[expect(
        clippy::disallowed_methods,
        reason = "net 是套接字的唯一入口（README §4）：跨会话的发送在这里连接"
    )]
    pub async fn connect(path: &Path) -> io::Result<UdsStream> {
        let stream = tokio::net::UnixStream::connect(path).await?;
        Ok(UdsStream::new(stream))
    }
}

#[cfg(not(unix))]
mod imp {
    use std::io;
    use std::path::Path;

    fn unsupported() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "Unix domain sockets are not available on this platform",
        )
    }

    pub struct UdsListener(());

    impl UdsListener {
        pub fn bind(_path: &Path) -> io::Result<Self> {
            Err(unsupported())
        }

        pub fn path(&self) -> &Path {
            Path::new("")
        }

        pub async fn accept(&self) -> io::Result<UdsStream> {
            Err(unsupported())
        }
    }

    pub struct UdsStream(());

    impl UdsStream {
        pub async fn read_line(&mut self) -> io::Result<Option<String>> {
            Err(unsupported())
        }

        pub async fn write_line(&mut self, _line: &str) -> io::Result<()> {
            Err(unsupported())
        }
    }

    pub async fn connect(_path: &Path) -> io::Result<UdsStream> {
        Err(unsupported())
    }
}

pub use imp::{UdsListener, UdsStream, connect};

/// 连接、写一行、读一行回复（对方没回就关闭时为 None）。
pub async fn request(path: &Path, line: &str) -> io::Result<Option<String>> {
    let mut stream = connect(path).await?;
    stream.write_line(line).await?;
    stream.read_line().await
}

/// 这个平台有没有 Unix 域套接字。
pub const fn supported() -> bool {
    cfg!(unix)
}

#[cfg(all(test, unix))]
#[path = "uds_tests.rs"]
mod tests;
