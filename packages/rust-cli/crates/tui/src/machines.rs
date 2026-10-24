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
}

/// 连接状态（R2 的状态机，§19.10，合并成界面要分的几种）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineStatus {
    /// 没连（清单里有，没连或已断开）。
    Disconnected,
    /// 部署中、握手中。
    Connecting,
    Ready,
    /// 连接断了，正在重连。
    Reconnecting {
        attempt: u32,
    },
    Failed(MachineFailure),
}

impl MachineStatus {
    /// 连着或正在连（enter 断开它，§21.3）。
    pub fn is_live(&self) -> bool {
        matches!(
            self,
            Self::Ready | Self::Connecting | Self::Reconnecting { .. }
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

    /// 连着时先断开，再从清单删掉。
    fn remove(&self, name: String) -> BoxFuture<'static, Result<(), MachineProblem>>;
}
