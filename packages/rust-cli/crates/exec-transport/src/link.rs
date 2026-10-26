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

/// 部署的进度（远端执行 F98，remote-exec.md §25.4.2）：第一次连接时状态行分出“上传执行器”一步。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProvisionStep {
    /// 开始下载执行器（F100，remote-exec.md §26.6：本机缓存里没有，从 npm 下载）。
    Downloading,
    /// 开始上传执行器：要上传的字节数。
    Uploading { bytes: u64 },
}

/// 让目标上有可用的执行器（SSH：探测、上传、校验；以后的云：建执行器）。结果是在目标上启动执行器的命令。
#[async_trait]
pub trait Provisioner: Send + Sync {
    async fn provision(&self) -> Result<Provisioned, ProvisionError>;

    /// 同 [`Provisioner::provision`]，进度交给 `progress`（F98）。缺省不报告进度。
    async fn provision_reporting(
        &self,
        progress: &(dyn Fn(ProvisionStep) + Send + Sync),
    ) -> Result<Provisioned, ProvisionError> {
        let _ = progress;
        self.provision().await
    }
}

/// 部署失败的原因（remote-exec.md §19.9）：连接状态机按它显示（R4 的 `/machines` 与状态行）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProvisionError {
    /// 到不了目标（ssh 起不来、认证失败、网络不通）：带 ssh 的 stderr 的最后几行。
    #[error("cannot reach {target}: {message}")]
    Unreachable { target: String, message: String },
    /// 目标的系统或架构不支持（只支持 Linux x86_64 / aarch64）。
    #[error("the remote is {os} {arch}; zcode can only run an executor on Linux x86_64 or aarch64")]
    Unsupported { os: String, arch: String },
    /// 目标上没有协议兼容的执行器，本机也没有可以上传的产物。
    #[error(
        "the remote has no compatible zcode executor and no Linux {target} build of zcode {version} is available to upload"
    )]
    NoArtifact { version: String, target: String },
    /// 目标上在跑的执行器说别的协议主版本；`busy` 时它还有工作，不能换。
    #[error("the remote executor {version} speaks protocol {protocol}{}", if *busy { " and is busy" } else { "" })]
    Incompatible {
        version: String,
        protocol: u32,
        busy: bool,
    },
    /// 上传失败（含远端算出的 sha256 不对）。
    #[error("uploading the executor failed: {0}")]
    Upload(String),
    /// 看不懂目标的回答。
    #[error("cannot understand the remote: {0}")]
    Probe(String),
    /// 目标上没有协议兼容的执行器，本机没有产物，下载也失败了（F100，remote-exec.md §26.2）：`reason` 是简短的原因。
    #[error(
        "the remote has no compatible zcode executor and downloading the Linux {target} build of zcode {version} failed: {reason}"
    )]
    DownloadFailed {
        version: String,
        target: String,
        reason: String,
    },
    /// 目标用 musl libc（例如 Alpine），也没有协议兼容的执行器：npm 的 Linux 包都要 glibc（F100，§26.5）。
    #[error(
        "the remote uses musl libc (for example Alpine); there is no zcode executor package for it"
    )]
    Musl,
    /// 下载的执行器在目标上跑不起来：它要的 glibc 比目标的新（F100，§26.5）。`glibc`：目标的 glibc 版本（探测不到时
    /// None）；`needs`：加载器报缺的最高版本，例如 `GLIBC_2.34`。
    #[error("the zcode executor needs {needs}, but the remote has {}", glibc.as_ref().map_or_else(|| "an older glibc".to_owned(), |version| format!("glibc {version}")))]
    OldGlibc {
        glibc: Option<String>,
        needs: String,
    },
}

/// 目标上启动执行器的命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provisioned {
    pub program: String,
    pub args: Vec<String>,
}

/// 执行器已经在目标上（本机的 loopback：就是本进程的二进制）。
pub struct Ready(pub Provisioned);

#[async_trait]
impl Provisioner for Ready {
    async fn provision(&self) -> Result<Provisioned, ProvisionError> {
        Ok(self.0.clone())
    }
}
