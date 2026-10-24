//! 线上的数据（docs/specs/rust-cli/remote-exec.md §18.3）。
//!
//! 与 kernel-api 的执行端口 DTO 逐字段对应，但不依赖 kernel-api（§15.1 原则 3）：内部 API 怎么改，线上格式不变。
//! 两边的换算只在 `env-remote`（客户端）与 `exec-server`（服务端）。路径与 ID 在线上都是字符串；字节字段是
//! [`crate::Blob`]（二进制段里的位置），不进 JSON。

pub mod command;
pub mod core;
pub mod files;
pub mod rewind;
pub mod shell;
