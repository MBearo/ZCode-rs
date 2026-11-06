//! 三个薄入口（交互界面、`-p`、app-server）与交互界面的服务（账号、机器、MCP、剪贴板、编辑器、工作区信任）。

pub(crate) mod app_server;
pub(crate) mod clipboard;
pub(crate) mod editor;
pub(crate) mod print;
pub(crate) mod tui;
pub(crate) mod tui_account;
pub(crate) mod tui_cockpit;
pub(crate) mod tui_inputs;
pub(crate) mod tui_machines;
pub(crate) mod tui_mcp;
pub(crate) mod tui_peek;
pub(crate) mod tui_screens;
pub(crate) mod tui_usage;
pub(crate) mod workspace_trust;
