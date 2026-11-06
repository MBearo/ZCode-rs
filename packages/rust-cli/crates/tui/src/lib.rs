//! 终端界面（docs/specs/rust-cli/tui.md）。
//!
//! 组件层复制自 `apps/zcode-host/crates/tui/src/ui`（不含 V4 部分），之后各自演进；runner 只经内核
//! 会话 API（`rust_cli_kernel_api::session_api`）驱动会话。
//!
//! 对外只有下面的再导出与 `machines`、`os`、`screens`、`services`、`trust`、`update` 六个入口模块；`ui`、`view`、
//! `runner` 是内部模块（架构整理 U5：以前三个都是 `pub mod`，rustc 的未使用检查看不到里面的死项）。
pub mod machines;
pub mod os;
pub(crate) mod runner;
pub mod screens;
pub mod services;
pub(crate) mod ui;
pub(crate) mod view;

/// 工作区信任的回答类型（在 tui-ports）：启动前的信任页（[`screens::ask_trust`]，tui-kit.md §2.3）与会话里切项目时的
/// 信任框（`ui::trust_ask`）。
pub mod trust {
    pub use rust_cli_tui_ports::trust::{Grants, TrustAnswer, TrustPrompt};
}

/// 自动更新的类型（在 tui-ports）。启动前的选择屏是 [`screens::ask_update`]（tui-kit.md §2.3）；会话里“有新版本”
/// 的提示用同一份 [`update::UpdateInfo`]（入口经 `UiConfig.update` 给出，update.md §7）。
pub mod update {
    pub use rust_cli_tui_ports::update::{UpdateChoice, UpdateInfo};
}

pub use os::{Os, WindowsRelease};
pub use runner::{Ending, Exit, Options, Screen, Startup, drive, run};
/// 入口与测试要用的界面类型：终端的颜色事实（颜色级别与终端背景）与墨色、一屏的行、界面语言、提问框的空闲超时、
/// 进入真实终端、会话里切项目时的信任框。
pub use ui::afk::QuestionTimeout;
pub use ui::frame::Frame;
pub use ui::i18n::Lang;
pub use ui::ink::Ink;
pub use ui::probe::{ColorLevel, Colors, Rgb};
pub use ui::terminal::enter as enter_terminal;
pub use ui::trust_view::Dialog as TrustDialog;
/// 会话视图（入口的回放测试与基准用）。
pub use view::{Body, Transcript};

/// 终端的列数（`zcode --help` 的折行宽度，cli-args.md §3）；不是终端或取不到时为 None。
pub fn terminal_columns() -> Option<u16> {
    crossterm::terminal::size().ok().map(|(columns, _)| columns)
}
pub use services::{
    Account, Authorize, AutoCompactCell, Clipboard, Configured, CopyRoute, Editor, Family,
    FamilyStatus, HistoryRecorder, ImageBytes, LocaleStore, LoginFailure, MemoryFile,
    PlanConnection, Preferences, Services, TeamStatus, UiConfig, UiLocale,
};
