//! 协议框架（docs/specs/rust-cli/app-protocol.md §3）：协议适配层的共用部分，只依赖 base 与 kernel-api，不认识任何
//! 一种协议。每种协议一个 crate（前端层），经这里的连接、会话桥与投递机制接内核的会话 API。
//!
//! - [`rpc`]：JSON-RPC 形状的消息与标准错误码；
//! - [`connection`]：按 LF 分帧的读循环、请求派发、写者、本端发出的请求；
//! - [`sessions`]：会话桥（每个会话开一次、订阅一次，事件按窗口成批交给协议）；
//! - [`delivery`]：主题日志与订阅者（谁该收到哪一段增量）；
//! - [`schema`]：入站参数的 JSON Schema 子集校验器（schema 文件由协议 crate 带）；
//! - [`serial`]：单写者（按顺序跑异步的工作，保证同一个主题的帧按计算的顺序写出）。

pub mod connection;
pub mod delivery;
pub mod rpc;
pub mod schema;
pub mod serial;
pub mod sessions;
