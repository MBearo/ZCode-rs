//! 一台远端机器的连接状态机（docs/specs/rust-cli/remote-exec.md §15.3、§19.10）：
//!
//! ```text
//!           connect                 部署好                握手成功
//!  Unconnected ──────► Provisioning ──────► Handshaking ──────► Ready ◄──────────────┐
//!      ▲                  │ 部署失败           │ 握手失败         │ 连接断了           │ 重连成功
//!      │                  ▼                    ▼                  ▼                    │
//!      │              Failed{error} ◄──────────────────── Reconnecting{attempt} ───────┘
//!      │                  │                     次数用完 / 被拒绝
//!      └── disconnect ────┴───（任何状态 disconnect 都回到 Unconnected）
//! ```
//!
//! - [`state`]：状态、事件与错误都是纯数据，转移是一个纯函数；
//! - [`machine`]：驱动——跑部署与握手，订阅 exec-client 的连接状态，把状态发到一个 `watch`。每个转移都由一个事件触发，
//!   不按超时改状态。
//!
//! 只依赖 exec-transport 的 `Provisioner` / `Connector` 与 exec-client：SSH 的实现（ssh-connect）由组装根经 [`Endpoint`]
//! 注入。机器清单 `machines.json` 与自动重连到上次的机器在 R3 / R4。

mod machine;
mod state;

pub use machine::{Endpoint, Machine};
pub use state::{MachineError, MachineEvent, MachineState, Welcomed, transition};

#[cfg(test)]
#[path = "machine_tests.rs"]
mod machine_tests;
#[cfg(test)]
#[path = "state_tests.rs"]
mod state_tests;
