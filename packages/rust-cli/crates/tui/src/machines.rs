//! 远端机器的服务（docs/specs/rust-cli/remote-exec.md §21.2）：入口（cli）实现，TUI 只经它看机器清单与连接状态、改清单、
//! 连接与断开。类型是 TUI 自己的：TUI 不认识 SSH、执行器与 machines crate（依赖注入，同 `services` 里的 `Preferences`）。
//!
//! 快照是入口内存里的事实拼出来的（读的时候不做 IO）；`changes` 只用来唤醒，醒来后再读快照。
use futures_util::future::BoxFuture;
use futures_util::stream::BoxStream;

/// 清单里的一台机器与它现在的状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineRow {
    pub name: String,
    /// ssh 目标（`~/.ssh/config` 的别名或 `user@host`）。
    pub ssh: String,
    /// 那台机器上的工作目录；没写时是远端的 home。
    pub working_directory: Option<String>,
    pub status: MachineStatus,
    /// 最近一次心跳的往返（毫秒）；就绪且量到过才有。
    pub latency_ms: Option<u64>,
    /// 握手时执行器报告的事实；本进程连上过才有。
    pub executor: Option<ExecutorInfo>,
}

/// 执行器报告的系统、架构与版本（握手的 welcome）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutorInfo {
    /// 执行器的写法：`linux`、`macos`……
    pub os: String,
    pub arch: String,
    pub version: String,
    /// 执行器报告了 `env` 能力位：每条连接会刷新 ssh 会话的 `SSH_AUTH_SOCK`（远端执行 F96，remote-exec.md §23.5）。
    pub refreshes_env: bool,
    /// 握手报告的远端 home（F98：清单没写工作目录时，询问框的“不再询问”写它，remote-exec.md §25.4.3）。
    pub home: Option<String>,
}

/// 第一次连接的哪一步（F98，remote-exec.md §25.4.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectStep {
    /// 探测（还不知道要不要上传）。
    Checking,
    /// 下载执行器（F100，remote-exec.md §26.6：本机缓存里没有）。
    Downloading,
    /// 上传执行器：字节数。
    Uploading { bytes: u64 },
    /// 握手：拉起执行器、hello / welcome。
    Handshaking,
}

/// 连接状态（R2 的状态机，§19.10，合并成界面要分的几种）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineStatus {
    /// 没连（清单里有，没连或已断开）。
    Disconnected,
    /// 部署中、握手中（F98 起分步）。
    Connecting(ConnectStep),
    Ready,
    /// 连接断了，正在重连（F96，remote-exec.md §23.4）：`attempt` 是下一次（等着时）或正在进行的是第几次；
    /// `retry_at_ms` 是下一次开始的时刻（墙上时间，同界面的时钟；正在连时没有）；`reason` 是最近一次的原因。
    Reconnecting {
        attempt: u32,
        retry_at_ms: Option<u64>,
        reason: String,
    },
    Failed(MachineFailure),
}

impl MachineStatus {
    /// 连着或正在连（enter 断开它，§21.3）。
    pub fn is_live(&self) -> bool {
        matches!(
            self,
            Self::Ready | Self::Connecting(_) | Self::Reconnecting { .. }
        )
    }

    /// 在等下一次重连（倒计时中）：可以“立即重试”（F98，remote-exec.md §25.4.1）。
    pub fn awaits_retry(&self) -> bool {
        matches!(
            self,
            Self::Reconnecting {
                retry_at_ms: Some(_),
                ..
            }
        )
    }
}

/// 连不上的原因（R2 的 `MachineError`，§19.10、§21.4）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineFailure {
    Unreachable(String),
    Unsupported {
        os: String,
        arch: String,
    },
    NoArtifact {
        version: String,
        target: String,
    },
    Incompatible {
        version: String,
        protocol: u32,
        busy: bool,
    },
    Upload(String),
    Probe(String),
    /// 下载执行器失败，远端也没有兼容的（F100，remote-exec.md §26.6）。
    DownloadFailed {
        version: String,
        target: String,
        reason: String,
    },
    /// 远端是 musl，没有执行器包（F100）。
    Musl,
    /// 执行器在远端跑不起来：要 `needs`，远端的 glibc 是 `glibc`（不知道时 None，F100）。
    OldGlibc {
        glibc: Option<String>,
        needs: String,
    },
    Refused(String),
    Lost(String),
}

/// 一个操作为什么没做成。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineProblem {
    Connection(MachineFailure),
    /// 名字不合规则（§20.3.1）。
    InvalidName(String),
    Exists(String),
    Missing(String),
    /// 清单文件读写失败：原因原样显示。
    File(String),
}

/// 连接与断开的结果：做了，或者本来就是那样。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Changed {
    Done,
    Already,
}

/// 入口实现的机器服务。
pub trait Machines: Send + Sync {
    /// 清单里的机器（按清单的顺序）与它们现在的状态。
    fn snapshot(&self) -> Vec<MachineRow>;

    /// 快照变了就出一项（清单变了、某台的状态或延迟变了）。
    fn changes(&self) -> BoxStream<'static, ()>;

    /// 写进清单（不连）。
    fn add(
        &self,
        name: String,
        ssh: String,
        working_directory: Option<String>,
    ) -> BoxFuture<'static, Result<(), MachineProblem>>;

    /// 连上（失败过的重新部署、握手）；连上或失败时结束。
    fn connect(&self, name: String) -> BoxFuture<'static, Result<Changed, MachineProblem>>;

    fn disconnect(&self, name: String) -> BoxFuture<'static, Result<Changed, MachineProblem>>;

    /// 立即重试（F98，remote-exec.md §25.4.1）：重连中、在等退避的机器马上试一次。`Already` 是没在等（正在连、没断）。
    fn retry(&self, name: String) -> BoxFuture<'static, Result<Changed, MachineProblem>>;

    /// 连着时先断开，再从清单删掉。
    fn remove(&self, name: String) -> BoxFuture<'static, Result<(), MachineProblem>>;
}
