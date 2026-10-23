//! 终端界面（docs/specs/rust-cli/tui.md）。
//!
//! 组件层复制自 `apps/zcode-host/crates/tui/src/ui`（不含 V4 部分），之后各自演进；runner 只经内核
//! 会话 API（`rust_cli_kernel_api::session_api`）驱动会话。
pub mod os;
pub mod runner;
pub mod services;
pub mod trust;
pub mod ui;
pub mod update;
pub mod view;

pub use os::{Os, WindowsRelease};
pub use runner::{Ending, Exit, Options, Screen, Startup, drive, run};

/// 终端的列数（`zcode --help` 的折行宽度，cli-args.md §3）；不是终端或取不到时为 None。
pub fn terminal_columns() -> Option<u16> {
    crossterm::terminal::size().ok().map(|(columns, _)| columns)
}
pub use services::{
    Account, Authorize, Clipboard, Configured, CopyRoute, Editor, Family, ImageBytes, LocaleStore,
    LoginFailure, MemoryFile, Preferences, Services, UiLocale, UiTheme,
};
