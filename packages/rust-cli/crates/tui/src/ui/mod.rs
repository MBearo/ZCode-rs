//! 终端界面的组件层（docs/specs/rust-cli/tui.md §3）：复制自 `apps/zcode-host/crates/tui/src/ui`，
//! 只持有草稿、滚动、视图等界面状态；会话内容来自 `view::Transcript`，每一帧按当前宽度重新排版。
pub mod afk;
pub mod agents;
mod agents_dock;
pub mod agents_tree;
pub mod app;
// `App` 的子状态（读位置、鼠标、按键、会话的快照，架构整理 U3）。
mod app_state;
pub mod ask_body;
pub mod at_panel;
pub mod at_search;
mod at_view;
pub mod auto_compact;
pub mod background;
pub mod bottom;
pub mod cells;
pub mod clipboard;
pub mod commands;
mod compose;
mod compose_input;
pub mod composer;
pub mod console;
pub mod context_count;
pub mod context_page;
pub mod context_tree;
pub mod detail;
pub mod diff_view;
pub mod diff_view_lines;
pub mod draft_layer;
pub mod draw;
pub mod export;
pub mod find;
pub mod find_text;
pub mod footer;
pub mod footer_stats;
pub mod fork;
pub mod page_copy;
pub mod page_frame;
// 一屏的行与按列的小操作（tui-kit.md §3.2）。
pub mod frame;
pub mod geom;
pub mod goal;
pub mod i18n;
// 请求的字（tui-kit.md §4.2，英文照 Puffin）。
pub mod i18n_asks;
pub mod i18n_input;
pub mod i18n_kit;
pub mod i18n_machines;
pub mod i18n_panels;
pub mod i18n_peers;
pub mod i18n_projects;
pub mod i18n_trust;
pub mod i18n_yolo;
// kit 的墨色角色（tui-kit.md §3.4、§3.8）。
pub mod ink;
pub mod input_box;
pub mod input_motion;
pub mod interaction;
pub mod machines_list;
pub mod status_motion;
// 待处理交互 → kit 的请求与回答。
pub mod interaction_ask;
pub mod interaction_view;
pub mod key_hint;
pub mod keyboard;
mod keys;
mod keys_burst;
// kit 类型的折行与截断（tui-kit.md §3.6、§3.8）。
pub mod doorway;
// 跨面板的排版常量（constants.md §5.3）。
pub mod layout;
// 单行输入的编辑：按字素删、粘贴压成一行（架构整理 U12）。
pub mod line_edit;
pub mod lines;
pub mod list;
mod locale;
pub mod login;
pub mod logo;
pub mod machines;
pub mod manage;
pub mod markdown;
pub mod mcp_control;
pub mod mention;
pub mod mouse;
pub mod notice;
pub mod notify;
pub mod overlay;
pub mod page;
pub mod pager;
pub mod pane;
pub mod panels;
pub mod paste_burst;
pub(crate) mod peer_view;
pub mod pick;
// Windows 的前景 / 背景色探测（工作线 F71）：只在 Windows 上调用，其他平台只有测试用。
pub mod cockpit;
mod cockpit_pane;
mod cockpit_view;
pub mod probe;
#[cfg_attr(
    not(windows),
    allow(dead_code, reason = "只在 Windows 上调用；其他平台只有测试用")
)]
mod probe_console;
pub mod program_status;
pub mod projects;
// 时长、时刻、计数与百分比的写法（constants.md §5.3）。
pub mod quantities;
pub mod retry_status;
pub mod rewind;
pub mod rewind_pick;
pub mod selection;
pub mod session_page;
pub mod session_rows;
pub mod sessions_list;
pub mod sessions_status;
pub mod sessions_steps;
// 设置页（tui-kit.md §4.2「设置」）。
pub mod cards;
pub mod settings;
pub mod shelf_rows;
pub mod skill_mention;
mod skill_view;
mod slash;
mod slash_more;
pub mod slash_theme;
pub mod stats_page;
pub mod task_card;
mod task_card_rows;
mod task_line;
pub mod tasks;
pub mod terminal;
#[cfg(test)]
mod test_support;
pub mod textarea;
pub mod transcript;
pub mod trust_ask;
pub mod trust_view;
pub mod usage_back;
pub mod usage_local;
pub mod usage_local_data;
pub mod usage_plans;
pub mod wheel;
pub mod yolo_confirm;

pub use slash::resolve_model;
