//! 状态、事件、错误与转移（remote-exec.md §19.10）。纯数据、纯函数。

use rust_cli_exec_client::ClientError;
use rust_cli_exec_transport::ProvisionError;

/// 握手完成时执行器报告的事实（`welcome` 里 R4 的状态行要的部分）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Welcomed {
    pub server_version: String,
    pub server_instance: String,
    pub os: String,
    pub arch: String,
}

/// 一台机器的连接状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineState {
    Unconnected,
    /// 部署中：探测、上传、校验。
    Provisioning,
    /// 部署中的上传（F98，remote-exec.md §25.4.2：第一次连接时状态行分出这一步）：要上传的字节数。
    Uploading {
        bytes: u64,
    },
    /// 握手中：拉起连接、hello / welcome。
    Handshaking,
    Ready(Welcomed),
    /// 连接断了，正在重连：已经开始了几次尝试（0：刚断）；下一次开始的时刻（等着时有，正在连时 None）；最近一次的
    /// 原因（F96，remote-exec.md §23.4：状态行写倒计时，`/machines` 写原因）。
    Reconnecting {
        attempt: u32,
        retry_at: Option<tokio::time::Instant>,
        reason: String,
    },
    Failed {
        error: MachineError,
    },
}

/// 重连的进度（exec-client 的 `ConnectionState::Reconnecting`，F96）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retry {
    pub attempt: u32,
    pub retry_at: Option<tokio::time::Instant>,
    pub reason: String,
}

/// 让状态变化的事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineEvent {
    Connect,
    /// 部署开始上传执行器（F98）。
    Uploading {
        bytes: u64,
    },
    Provisioned,
    ProvisionFailed(MachineError),
    Welcomed(Welcomed),
    HandshakeFailed(MachineError),
    LinkLost,
    Retrying(Retry),
    Reattached(Welcomed),
    GaveUp(MachineError),
    Disconnect,
}

/// `Failed` 的原因（由 `ProvisionError` 与 exec-client 的 `ClientError` 换算）。R4 的 `/machines` 与状态行按它显示。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MachineError {
    #[error("unreachable: {0}")]
    Unreachable(String),
    #[error("{os} {arch} is not supported (only Linux x86_64 and aarch64)")]
    Unsupported { os: String, arch: String },
    #[error("no Linux {target} build of zcode {version} to upload")]
    NoArtifact { version: String, target: String },
    #[error("the remote executor {version} speaks protocol {protocol}{}", if *busy { " and is busy" } else { "" })]
    Incompatible {
        version: String,
        protocol: u32,
        busy: bool,
    },
    #[error("upload failed: {0}")]
    Upload(String),
    #[error("cannot understand the remote: {0}")]
    Probe(String),
    /// 执行器拒绝了握手（协议、鉴权、旧连接）。
    #[error("refused: {0}")]
    Refused(String),
    /// 连接断了、不再重连。
    #[error("connection lost: {0}")]
    Lost(String),
}

impl From<ProvisionError> for MachineError {
    fn from(error: ProvisionError) -> Self {
        match error {
            ProvisionError::Unreachable { message, .. } => Self::Unreachable(message),
            ProvisionError::Unsupported { os, arch } => Self::Unsupported { os, arch },
            ProvisionError::NoArtifact { version, target } => Self::NoArtifact { version, target },
            ProvisionError::Incompatible {
                version,
                protocol,
                busy,
            } => Self::Incompatible {
                version,
                protocol,
                busy,
            },
            ProvisionError::Upload(message) => Self::Upload(message),
            ProvisionError::Probe(message) => Self::Probe(message),
        }
    }
}

impl From<ClientError> for MachineError {
    fn from(error: ClientError) -> Self {
        match error {
            ClientError::Connect(message) => Self::Unreachable(message),
            ClientError::Refused { message, .. } => Self::Refused(message),
            ClientError::Protocol(message) => Self::Refused(message),
            // 重连被目标拒绝（ssh 认证失败等，F96）：同部署时的“到不了”。
            ClientError::Rejected(message) => Self::Unreachable(message),
            ClientError::NotSent(inner) => Self::from(*inner),
            other => Self::Lost(other.to_string()),
        }
    }
}

/// 转移：合法时给出新状态，不合法的组合是 None（调用方记日志，状态不变）。
pub fn transition(state: &MachineState, event: MachineEvent) -> Option<MachineState> {
    use MachineEvent as E;
    use MachineState as S;
    Some(match (state, event) {
        (_, E::Disconnect) => S::Unconnected,
        (S::Unconnected | S::Failed { .. }, E::Connect) => S::Provisioning,
        (S::Provisioning, E::Uploading { bytes }) => S::Uploading { bytes },
        (S::Provisioning | S::Uploading { .. }, E::Provisioned) => S::Handshaking,
        (S::Provisioning | S::Uploading { .. }, E::ProvisionFailed(error))
        | (S::Handshaking, E::HandshakeFailed(error)) => S::Failed { error },
        (S::Handshaking, E::Welcomed(welcomed)) => S::Ready(welcomed),
        (S::Ready(_), E::LinkLost) => S::Reconnecting {
            attempt: 0,
            retry_at: None,
            reason: String::new(),
        },
        (S::Reconnecting { .. }, E::Retrying(retry)) => S::Reconnecting {
            attempt: retry.attempt,
            retry_at: retry.retry_at,
            reason: retry.reason,
        },
        // 重连成功；已经是 Ready 时更新事实（watch 只留最新值，中间的“断开”可能没看到）。
        (S::Reconnecting { .. } | S::Ready(_), E::Reattached(welcomed)) => S::Ready(welcomed),
        (S::Ready(_) | S::Reconnecting { .. }, E::GaveUp(error)) => S::Failed { error },
        _ => return None,
    })
}
