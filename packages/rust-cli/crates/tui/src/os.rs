//! 运行的操作系统（入口注入，docs/specs/rust-cli/tui-input.md §9、tui.md §7.4，修复线 F65）：类型在 `rust-cli-tui-ports`
//! （tui-kit.md §2.2），这里再导出。
pub use rust_cli_tui_ports::os::{Os, WindowsRelease};
