//! 远端执行器的传输（docs/specs/rust-cli/remote-exec.md §4、§15.3、§18.2）：只搬帧，不认识帧的内容（不依赖
//! `exec-wire`）。
//!
//! - [`link`]：trait `Link`（帧的收发两端）、`Listener`（执行器侧收连接）、`Connector`（到达一个目标得到
//!   Link）、`Provisioner`（让目标上有可用的执行器）；
//! - [`stream`]：字节流上的帧（u32 BE 长度前缀 + 一帧），stdio 与以后的 unix socket 共用；
//! - [`stdio`]：经 proc 拉起子进程，拿它的 stdin / stdout（loopback 与以后的 ssh）；
//! - [`memory`]：同进程的内存双工与“网络”（测试与同进程的 loopback）；
//! - [`fault`]：故障注入的包装（丢帧、延迟、断开、扣住一帧放到下一帧之后）。

pub mod fault;
pub mod link;
pub mod memory;
pub mod stdio;
pub mod stream;

pub use link::{
    Connector, FrameSink, FrameSource, Link, LinkError, Listener, ProvisionError, Provisioned,
    Provisioner, Ready,
};

/// 单帧的缺省上限（§18.2）：超过就断开。exec-wire 的分片保证正常的帧远小于它。
pub const MAX_FRAME: usize = 4 * 1024 * 1024;

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
