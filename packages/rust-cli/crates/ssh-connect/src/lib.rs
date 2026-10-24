//! SSH 的连接与部署（docs/specs/rust-cli/remote-exec.md §8、§15.3、§19.9）：用本机的 OpenSSH 客户端（`ssh` 进程，经
//! proc 起），不内置 SSH 库，所以 `~/.ssh/config`、ssh-agent、跳板机照常生效；只用免交互认证（`BatchMode=yes`）。
//!
//! - [`SshConnector`]：`ssh -T … <目标> '<执行器>' executor --stdio`，得到 stdio Link（exec-transport 的 `StdioConnector`）；
//! - [`SshProvisioner`]：探测远端的系统、架构与已有的执行器（含 npm 装的），按 §19.9 的表选用，或上传本机同版本的
//!   静态 Linux 产物并校验 sha256。
//!
//! 只认识 exec-transport 的 `Connector` / `Provisioner`，不认识协议（exec-wire）与状态机（machines）：要的版本与协议
//! 主版本由组装根给（[`Wanted`]）。

mod artifact;
mod choose;
mod connector;
mod probe;
mod provision;
mod remote;
mod target;

pub use artifact::Arch;
pub use choose::{Plan, Wanted, choose};
pub use connector::SshConnector;
pub use probe::{Candidate, Probe, Report, Resident};
pub use provision::SshProvisioner;
pub use target::{SshTarget, quote};

#[cfg(test)]
#[path = "choose_tests.rs"]
mod choose_tests;
#[cfg(test)]
#[path = "probe_tests.rs"]
mod probe_tests;
#[cfg(all(test, unix))]
#[path = "provision_tests.rs"]
mod provision_tests;
