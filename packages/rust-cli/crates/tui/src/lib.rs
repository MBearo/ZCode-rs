//! 终端界面（docs/specs/rust-cli/tui.md）。
//!
//! 组件层复制自 `apps/zcode-host/crates/tui/src/ui`（不含 V4 部分），之后各自演进；runner 只经内核
//! 会话 API（`rust_cli_kernel_api::session_api`）驱动会话。
//!
//! 对外只有下面的再导出与 `machines`、`os`、`services`、`trust`、`update` 五个入口模块；`ui`、`view`、`runner`
//! 是内部模块（架构整理 U5：以前三个都是 `pub mod`，rustc 的未使用检查看不到里面的死项）。
pub mod machines;
pub mod os;
pub(crate) mod runner;
pub mod services;
pub mod trust;
pub(crate) mod ui;
pub mod update;
pub(crate) mod view;

pub use os::{Os, WindowsRelease};
pub use runner::{Ending, Exit, Options, Screen, Startup, drive, run};
/// 入口与测试要用的界面类型：配色（颜色级别与终端背景）、界面语言、提问框的空闲超时、进入真实终端。
pub use ui::afk::QuestionTimeout;
pub use ui::i18n::Lang;
pub use ui::palette::{ColorLevel, Palette, Rgb};
pub use ui::terminal::enter as enter_terminal;
/// 会话视图（入口的回放测试与基准用）。
pub use view::{Body, Transcript};

/// 终端的列数（`zcode --help` 的折行宽度，cli-args.md §3）；不是终端或取不到时为 None。
pub fn terminal_columns() -> Option<u16> {
    crossterm::terminal::size().ok().map(|(columns, _)| columns)
}
pub use services::{
    Account, Authorize, Clipboard, Configured, CopyRoute, Editor, Family, HistoryRecorder,
    ImageBytes, LocaleStore, LoginFailure, MemoryFile, Preferences, Services, UiConfig, UiLocale,
    UiTheme,
};
