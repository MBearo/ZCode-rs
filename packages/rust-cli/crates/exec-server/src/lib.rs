//! 远端执行器的服务端（docs/specs/rust-cli/remote-exec.md §15.3、§18）：收连接、握手与鉴权（`Authenticator`）、工作目录
//! 围栏、操作账本（`ExecLedger`），把请求派发给注入的 `MachineEnv`（本机是 env-local：远端的执行逻辑就是同一份代码）。
//!
//! 线上的类型与 kernel-api 的换算只在这里（服务端）与 `env-remote`（客户端）。

mod auth;
mod calls;
mod connection;
mod convert_in;
mod convert_out;
mod dispatch;
mod env;
mod fence;
mod ledger;
mod mcp;
mod server;

pub use auth::{AllowAll, Authenticator, Principal};
pub use fence::Fence;
pub use ledger::{Begun, CallKey, CallKind, ExecLedger, LedgerError, MemoryLedger};
pub use server::{Activity, ExecServer, ServerConfig, ServerError};

#[cfg(test)]
#[path = "env_tests.rs"]
mod env_tests;
#[cfg(test)]
#[path = "mcp_tests.rs"]
mod mcp_tests;
#[cfg(test)]
#[path = "resident_tests.rs"]
mod resident_tests;
#[cfg(test)]
#[path = "server_tests.rs"]
mod server_tests;
#[cfg(test)]
#[path = "stream_tests.rs"]
mod stream_tests;
#[cfg(test)]
mod test_support;
