//! 握手与能力位（docs/specs/rust-cli/remote-exec.md §18.3、§18.4）。

use std::collections::BTreeSet;

use rust_cli_base::secret::RedactedString;
use serde::{Deserialize, Serialize};

use crate::dto::core::PathStyle;

/// 协议主版本：两边不同就拒绝；小改动只加能力位与可选字段，不比较版本号大小。
pub const PROTOCOL: u32 = 1;

/// 缺省的心跳间隔（毫秒，§18.5）。
pub const HEARTBEAT_MS: u64 = 10_000;

/// 连续这么多个心跳间隔没收到任何帧，就当连接断了（§18.5；客户端与服务端都按它）。
pub const HEARTBEAT_MISSES: u32 = 3;

/// 一个流最多有几项没有 ack（§18.5）：服务端到了就不再从端口取，客户端按它留缓冲。
pub const STREAM_WINDOW: u64 = 64;

/// 按端口分组的能力位。线上写名字（看得懂、能忽略不认识的）；“位”是语义：有或没有。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Capability {
    Info,
    Files,
    Search,
    Commands,
    Shell,
    Background,
    Checkpoints,
    Pdf,
    /// MCP 的管道（R5，§15.9、§22.4）：R5 的执行器（常驻与 `--standalone`）报告它，老执行器不报告。
    Mcp,
    /// 按客户端实例推送环境变量（F96，remote-exec.md §23.5）：`env.update` 与 `hello.session_env`。F96 的执行器报告它。
    Env,
}

impl Capability {
    /// `MachineEnv` 的 8 个端口（R1 的执行器全部报告）。
    pub const PORTS: [Self; 8] = [
        Self::Info,
        Self::Files,
        Self::Search,
        Self::Commands,
        Self::Shell,
        Self::Background,
        Self::Checkpoints,
        Self::Pdf,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Files => "files",
            Self::Search => "search",
            Self::Commands => "commands",
            Self::Shell => "shell",
            Self::Background => "background",
            Self::Checkpoints => "checkpoints",
            Self::Pdf => "pdf",
            Self::Mcp => "mcp",
            Self::Env => "env",
        }
    }
}

/// 一方报告的能力位：名字的集合。不认识的名字保留（往返不丢），判断时忽略。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Capabilities(BTreeSet<String>);

impl Capabilities {
    pub fn of(capabilities: impl IntoIterator<Item = Capability>) -> Self {
        Self(
            capabilities
                .into_iter()
                .map(|capability| capability.as_str().to_owned())
                .collect(),
        )
    }

    /// `MachineEnv` 的全部端口。
    pub fn ports() -> Self {
        Self::of(Capability::PORTS)
    }

    /// 全部端口加上 MCP 的管道（R5 的执行器，§22.4）。
    pub fn ports_and_mcp() -> Self {
        Self::of(Capability::PORTS.into_iter().chain([Capability::Mcp]))
    }

    /// 现在的执行器报告的全部能力位：端口、MCP 的管道与环境变量推送（F96，§23.5）。
    pub fn executor() -> Self {
        Self::of(
            Capability::PORTS
                .into_iter()
                .chain([Capability::Mcp, Capability::Env]),
        )
    }

    pub fn contains(&self, capability: Capability) -> bool {
        self.0.contains(capability.as_str())
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(String::as_str)
    }
}

/// 客户端的第一帧。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u32,
    pub client_version: String,
    /// 客户端实例：操作按（实例，调用 ID）记账，重连时不变。
    pub client_instance: String,
    /// 这个实例的第几次连接：新的替换旧的，比已知的小就拒绝。
    pub epoch: u64,
    pub capabilities: Capabilities,
    /// 客户端的心跳间隔（毫秒）：服务端三个间隔没收到任何帧就关掉这条连接。没有时按 [`HEARTBEAT_MS`]。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heartbeat_ms: Option<u64>,
    /// 不透明的凭据（云端的令牌；SSH 与 loopback 下没有）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials: Option<RedactedString>,
    /// 这条连接所在的 ssh 会话的环境（白名单里的名字；这条会话没有时是 null）：SSH 的代理在转发 hello 时加上，客户端
    /// 自己不写。执行器按 `env.update` 的合并处理，先于这条连接上的任何请求（F96，remote-exec.md §23.5）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_env: Option<crate::dto::env::EnvVars>,
}

/// 服务端接受握手。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Welcome {
    pub protocol: u32,
    pub server_version: String,
    /// 执行器的实例：变了说明执行器重启过（§18.5）。
    pub server_instance: String,
    pub os: String,
    pub arch: String,
    pub path_style: PathStyle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    pub capabilities: Capabilities,
    /// 一条消息（拼回之后）的上限，字节。
    pub max_message: u64,
    /// 账本的身份（R2，remote-exec.md §19.8）：执行器重启后还是同一份账本时相同。客户端看到实例换了、账本相同时，
    /// 把有副作用的调用也补发，由账本决定结果；没有（内存账本、老的执行器）时按 R1 的规则。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ledger: Option<String>,
}

/// 服务端拒绝握手，随后断开。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refuse {
    pub code: RefuseCode,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefuseCode {
    /// 主版本不同。
    Protocol,
    /// `Authenticator` 不放行。
    Auth,
    /// 同一实例更新的连接已经在了。
    Stale,
    /// 以后加的原因（旧客户端按普通的拒绝处理）。
    #[serde(other)]
    Other,
}
