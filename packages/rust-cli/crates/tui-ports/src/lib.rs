//! 终端界面（`tui`）与入口（cli）共用的端口（docs/specs/rust-cli/tui-kit.md §2.2）：入口实现、界面调用的服务 trait 与
//! 它们的类型。只有类型与 trait：没有 IO、没有画面、不读环境。界面自己多出来的端口（剪贴板）留在界面的 crate。
//! Puffin 的三个小文件（`Stores`）旧界面也要（`theme.json`），在这里（docs/specs/rust-cli/tui-kit.md §2.1）。
//!
//! 为什么单独一个 crate（当时有两套界面）：两套界面要 cli 的同一批服务，端口的定义只能有一份；放在任一界面 crate 里另一个就得依赖它
//! （frontend 不能依赖 frontend，README §3.1），各复制一份则 cli 要写两套实现或在同形的类型之间换算。
#![forbid(unsafe_code)]

pub mod machines;
pub mod os;
pub mod services;
pub mod trust;
pub mod update;

pub use os::{Os, WindowsRelease};
