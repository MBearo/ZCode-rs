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
    /// 握手中：拉起连接、hello / welcome。
    Handshaking,
    Ready(Welcomed),
    /// 连接断了，正在重连（第几次尝试；0：刚断）。
    Reconnecting {
        attempt: u32,
    },
    Failed {
        error: MachineError,
    },
}

/// 让状态变化的事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineEvent {
    Connect,
    Provisioned,
    ProvisionFailed(MachineError),
    Welcomed(Welcomed),
    HandshakeFailed(MachineError),
    LinkLost,
    Retrying(u32),
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
        (S::Provisioning, E::Provisioned) => S::Handshaking,
        (S::Provisioning, E::ProvisionFailed(error))
        | (S::Handshaking, E::HandshakeFailed(error)) => S::Failed { error },
        (S::Handshaking, E::Welcomed(welcomed)) => S::Ready(welcomed),
        (S::Ready(_), E::LinkLost) => S::Reconnecting { attempt: 0 },
        (S::Reconnecting { .. }, E::Retrying(attempt)) => S::Reconnecting { attempt },
        // 重连成功；已经是 Ready 时更新事实（watch 只留最新值，中间的“断开”可能没看到）。
        (S::Reconnecting { .. } | S::Ready(_), E::Reattached(welcomed)) => S::Ready(welcomed),
        (S::Ready(_) | S::Reconnecting { .. }, E::GaveUp(error)) => S::Failed { error },
        _ => return None,
    })
}
