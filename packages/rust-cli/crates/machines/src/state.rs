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
    /// 部署中的下载（F100，remote-exec.md §26.6）：本机缓存里没有执行器，从 npm 下载。
    Downloading,
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
    /// 部署开始下载执行器（F100）。
    Downloading,
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
    #[error("{os} {arch} is not supported (only Linux x86_64 / aarch64 and macOS arm64)")]
    Unsupported { os: String, arch: String },
    #[error("no {target} build of zcode {version} to upload")]
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
    /// 下载执行器失败，远端也没有兼容的（F100，remote-exec.md §26.6）。
    #[error("downloading the {target} build of zcode {version} failed: {reason}")]
    DownloadFailed {
        version: String,
        target: String,
        reason: String,
    },
    /// 远端是 musl，没有执行器包（F100）。
    #[error("the remote uses musl libc; there is no zcode executor package for it")]
    Musl,
    /// 执行器在远端跑不起来：远端的 glibc 太旧（F100）。
    #[error("the executor needs {needs}, but the remote has {}", glibc.as_ref().map_or_else(|| "an older glibc".to_owned(), |version| format!("glibc {version}")))]
    OldGlibc {
        glibc: Option<String>,
        needs: String,
    },
    /// 本机的版本不在 npm 上，npm 的 latest 说别的协议（F103，remote-exec.md §27.7）。
    #[error(
        "zcode {version} (protocol {protocol}) is not on npm, and the latest executor there, {latest}, speaks protocol {latest_protocol}"
    )]
    LatestIncompatible {
        version: String,
        protocol: u32,
        latest: String,
        latest_protocol: u32,
        target: String,
    },
    /// 这个平台没有发布的执行器包（F107，remote-exec.md §28.2：Intel Mac）。
    #[error("there is no zcode executor package for the remote's platform ({package})")]
    NoPackage { package: String },
    /// 上传的执行器在远端被系统杀掉：签名无效（F107，remote-exec.md §28.5）。
    #[error("the uploaded executor was killed on the remote: its signature is not valid")]
    Unsigned,
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
            ProvisionError::DownloadFailed {
                version,
                target,
                reason,
            } => Self::DownloadFailed {
                version,
                target,
                reason,
            },
            ProvisionError::Musl => Self::Musl,
            ProvisionError::OldGlibc { glibc, needs } => Self::OldGlibc { glibc, needs },
            ProvisionError::LatestIncompatible {
                version,
                protocol,
                latest,
                latest_protocol,
                target,
            } => Self::LatestIncompatible {
                version,
                protocol,
                latest,
                latest_protocol,
                target,
            },
            ProvisionError::NoPackage { package } => Self::NoPackage { package },
            ProvisionError::Unsigned => Self::Unsigned,
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
        // 部署的分步（F98 上传、F100 下载）：探测 → [下载] → [上传] → 握手；下载失败、改用协议相同的候选时从下载直接
        // 进握手。
        (S::Provisioning, E::Downloading) => S::Downloading,
        (S::Provisioning | S::Downloading, E::Uploading { bytes }) => S::Uploading { bytes },
        (S::Provisioning | S::Downloading | S::Uploading { .. }, E::Provisioned) => S::Handshaking,
        (S::Provisioning | S::Downloading | S::Uploading { .. }, E::ProvisionFailed(error))
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
