//! SSH 的连接与部署（docs/specs/rust-cli/remote-exec.md §8、§15.3、§19.9）：用本机的 OpenSSH 客户端（`ssh` 进程，经
//! proc 起），不内置 SSH 库，所以 `~/.ssh/config`、ssh-agent、跳板机照常生效；只用免交互认证（`BatchMode=yes`）。
//!
//! - [`SshConnector`]：`ssh -T … <目标> '<执行器>' executor --stdio`，得到 stdio Link（exec-transport 的 `StdioConnector`）；
//! - [`SshProvisioner`]：探测远端的系统、架构与已有的执行器（含 npm 装的），按 §19.9 的表选用，或上传本机同版本的
//!   静态产物并校验 sha256；没有时经注入的 [`ArtifactSource`] 拿 npm 的平台包（F100，§26）；本机的版本不在 npm 上时
//!   退到 npm 的 latest，试跑核对协议（F103，§27）。远端的平台是 Linux x86_64 / aarch64 或 macOS arm64（F107，§28）。
//!
//! 只认识 exec-transport 的 `Connector` / `Provisioner`，不认识协议（exec-wire）与状态机（machines）：要的版本与协议
//! 主版本由组装根给（[`Wanted`]）。

mod artifact;
mod choose;
mod connector;
mod probe;
mod provision;
mod remote;
mod source;
mod target;

pub use artifact::Platform;
pub use choose::{Plan, Wanted, choose};
pub use connector::SshConnector;
pub use probe::{Candidate, Libc, Probe, Report, Resident};
pub use provision::SshProvisioner;
pub use source::{ArtifactSource, DownloadError};
pub use target::{SshTarget, quote};

#[cfg(test)]
#[path = "artifact_tests.rs"]
mod artifact_tests;
#[cfg(test)]
#[path = "choose_tests.rs"]
mod choose_tests;
#[cfg(test)]
#[path = "probe_tests.rs"]
mod probe_tests;
#[cfg(all(test, unix))]
#[path = "provision_latest_tests.rs"]
mod provision_latest_tests;
#[cfg(all(test, unix))]
#[path = "provision_macos_tests.rs"]
mod provision_macos_tests;
#[cfg(all(test, unix))]
#[path = "provision_source_tests.rs"]
mod provision_source_tests;
#[cfg(all(test, unix))]
#[path = "provision_tests.rs"]
mod provision_tests;
