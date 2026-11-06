//! 本机才有的文件与身份（docs/specs/rust-cli/platform.md §1）：Node 共用的用户级配置 `~/.zcode/cli/config.json`、App 设置
//! `setting.json` 的只读视图、hook 的信任记录、插件清单、设备身份 deviceMid 与 TUI 的输入历史。都与 [`crate::Platform`] 的
//! 账号与 Provider 无关，只有入口（cli）用；前端经入口注入的服务间接用到（tui 不依赖 platform）。
//!
//! F133（审查 E7）：以前这些模块与账号、Provider 平铺在 crate 根下；收进这一个目录，看路径就知道哪些只在本机（云上的
//! controller 不在用户的机器上时，这些要换来源，deployment.md）。不拆 crate。

pub(crate) mod cli_config;
mod device;
mod history;
mod hook_trust;
pub mod plugin_inventory;
mod settings;

pub use cli_config::{
    ConfigFile, ConfigWriteError, compact_at_text, mcp_enabled_text, read_cli_config,
    write_compact_at, write_mcp_enabled, write_ui_locale, write_ui_theme,
};
pub use device::{DeviceMid, NewUuid};
pub use history::{History, Recorder};
pub use hook_trust::{HookTrustStore, TrustError, TrustLoad, TrustRecord};
pub use settings::Settings;
