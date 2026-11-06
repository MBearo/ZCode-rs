//! 远端机器的服务（docs/specs/rust-cli/remote-exec.md §21.2）：trait 与类型在 `rust-cli-tui-ports`
//! （tui-kit.md §2.2），这里再导出；cli 实现，TUI 只经它看机器清单与连接状态、改清单、连接与断开。
pub use rust_cli_tui_ports::machines::*;
