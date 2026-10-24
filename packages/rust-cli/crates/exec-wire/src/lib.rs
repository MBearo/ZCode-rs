//! 远端执行器的线上协议 `zcode-exec/1`（docs/specs/rust-cli/remote-exec.md §7、§18）：消息、能力位与帧的编解码。
//!
//! 只有数据，不做 IO；不认识传输（stdio、以后的 gRPC），也不依赖 kernel-api（§15.1 原则 3）。
//! - [`admin`]：管理帧（常驻执行器的状态与停止，§19.6）；
//! - [`handshake`]：`hello` / `welcome` / `refuse`、主版本与按端口分组的能力位；
//! - [`request`] / [`reply`]：`MachineEnv` 的每个方法一种请求、一种回复，名字与属性在 [`op`]；
//! - [`envelope`]：帧头（调用 ID、流的 `seq`、批量 ack、关闭流、心跳）与线上的错误；
//! - [`dto`]：参数与结果的线上类型；字节字段是 [`Blob`]，字节在帧的二进制段；
//! - [`frame`]：帧的内容、分片与拼回。

pub mod admin;
pub mod blob;
pub mod dto;
pub mod envelope;
pub mod frame;
pub mod handshake;
pub mod op;
pub mod reply;
pub mod request;

pub use blob::{Blob, Body, BodyBuilder};

#[cfg(test)]
#[path = "frame_tests.rs"]
mod frame_tests;
#[cfg(test)]
mod test_support;
#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
