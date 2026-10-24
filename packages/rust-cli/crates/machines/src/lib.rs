//! 远端机器（docs/specs/rust-cli/remote-exec.md §15.3、§19.10、§20.3）：机器清单与一台机器的连接状态机。
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
//! - [`list`]：机器清单 `~/.zcode/rust-cli/machines.json`（R3）：名字、ssh 目标、工作目录、上次是否连着；
//! - [`state`]：状态、事件与错误都是纯数据，转移是一个纯函数；
//! - [`machine`]：驱动——跑部署与握手，订阅 exec-client 的连接状态，把状态发到一个 `watch`。每个转移都由一个事件触发，
//!   不按超时改状态。
//!
//! 只依赖 exec-transport 的 `Provisioner` / `Connector` 与 exec-client：SSH 的实现（ssh-connect）由组装根经 [`Endpoint`]
//! 注入。

pub mod list;
mod machine;
mod state;

pub use list::{MachineEntry, MachineList, MachinesError};
pub use machine::{Endpoint, Machine};
pub use state::{MachineError, MachineEvent, MachineState, Welcomed, transition};

#[cfg(test)]
#[path = "machine_tests.rs"]
mod machine_tests;
#[cfg(test)]
#[path = "state_tests.rs"]
mod state_tests;
