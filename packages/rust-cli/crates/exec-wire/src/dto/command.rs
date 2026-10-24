//! 跑命令的线上类型（kernel-api `env_command`、`env_shell` 里起命令用到的部分）。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::Blob;
use crate::dto::core::Outcome;

/// 启动一条命令（kernel-api `StartCommand`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartCommand {
    pub command: String,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    pub task: String,
    pub shell: ShellKind,
    pub cwd_scope: CwdScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<BackgroundCommand>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<SandboxPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook: Option<HookIo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_timeout: Option<TimeoutBackground>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub socket: Option<MonitorSocket>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellKind {
    Bash,
    PowerShell,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CwdScope {
    Pinned,
    Session { roots: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackgroundCommand {
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use: Option<String>,
    pub kind: BackgroundKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundKind {
    Bash,
    Monitor,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxPolicy {
    pub filesystem: FilesystemPolicy,
    pub network: NetworkPolicy,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesystemPolicy {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow_write: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny_write: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny_read: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NetworkPolicy {
    Unrestricted,
    LocalOnly {
        #[serde(default)]
        allow_local_binding: bool,
    },
    Domains {
        allowed: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        denied: Vec<String>,
    },
}

/// hook 命令的输入输出（kernel-api `HookIo`）：stdin 可能很大（工具输入），放在二进制段。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookIo {
    pub stdin: Blob,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeoutBackground {
    pub command: BackgroundCommand,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorSocket {
    pub url: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub protocols: Vec<String>,
}

/// 命令输出事件（kernel-api `CommandEvent`）：输出的字节在二进制段。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CommandEvent {
    Started {
        output: String,
    },
    Output {
        stream: OutputStream,
        data: Blob,
        cursor: u64,
    },
    Backgrounded,
    Finished {
        outcome: Outcome,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        #[serde(default)]
        timed_out: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd_reset: Option<String>,
        #[serde(default)]
        output_bytes: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_limit: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputStream {
    Stdout,
    Stderr,
}
