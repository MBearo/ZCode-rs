//! Node CLI 记录格式的纯函数（docs/specs/rust-cli/node-store.md；F128，审查 X5）：内核的事实与消息 ↔ Node 的 message /
//! part / session_entry 的 JSON，活动分支与压缩边界，冷读取的消息重建。不碰 SQLite、文件与时钟：仓储与事务在
//! node-store，以后别的存储适配器（例如云端的 PG）复用同一份记录格式。
//!
//! - [`records`]：消息、part、entry 的形状（Node 键序）；[`keys`]：本工程独有的 entry 的名字与键；
//! - [`codecs`]：行的 `data` 列 ↔ 带 id 的 JSON；[`row`]：一条消息与它的 part；
//! - [`branch`]：活动分支（回退游标、压缩边界）；[`hydrate`]：Node 记录 → 内核消息；
//! - [`tool_parts`] / [`tool_content`]：工具调用与结果的 part；[`notices`]：本工程写的合成消息；[`compaction`]：压缩边界；
//! - [`mode`]：权限模式与 Node 的协作模式；[`artifacts`]：artifact 的 URI。

pub mod artifacts;
pub mod branch;
pub mod codecs;
pub mod compaction;
pub mod hydrate;
pub mod keys;
pub mod mode;
pub mod notices;
pub mod records;
pub mod row;
pub mod tool_content;
pub mod tool_parts;
