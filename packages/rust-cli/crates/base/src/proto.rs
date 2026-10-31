//! 协议与系统的固定值（docs/specs/rust-cli/constants.md §2）：HTTP 的状态码、原因短语与通用头名，MIME 类型，
//! Unix 文件权限位，Linux `/proc/<pid>/stat` 的字段，进程的退出码与信号。都是公开标准里的值，与产品无关；几个 crate 共用，放在最底层
//! （hooks、tools 不依赖 net，net 的 `header` 从这里重新导出）。

pub mod exit;
pub mod http;
pub mod mime;
pub mod mode;
pub mod procfs;

#[cfg(test)]
#[path = "proto_tests.rs"]
mod tests;
