//! 两套界面共用的小部件（docs/specs/rust-cli/tui-kit.md §4.4）：从 Puffin 的壳搬来的无状态排版与动效——淡入、工作的行
//! （Lattice）、笔、流式的播放头、欢迎块的图标、按键提示的槽与按键表的排版。只依赖 kit 自己（`render`、`theme`、`app`、
//! `markdown`），不带壳的类型；壳与旧界面都从这里调。
pub mod fade;
pub mod key_hints;
pub mod lattice;
pub mod live_speed;
pub mod pen;
pub mod playhead;
// 命令现在不能运行的原因：两套界面共用的规则，入参是中性的事实。
pub mod command_reasons;
// transcript 的内容（工具行、收拢、名册、失败、通知、shell 块、欢迎块）：移植自 Puffin 的壳，入参是中性的数据。
pub mod entries;
pub mod failure_block;
pub mod fit;
pub mod gather;
pub mod notices;
pub mod roster;
pub mod shell_block;
pub mod subagent_cols;
pub mod todo_step;
pub mod tool_row;
pub mod tools;
pub mod user_slab;
pub mod welcome;

#[cfg(test)]
mod key_hints_tests;
#[cfg(test)]
mod lattice_tests;
#[cfg(test)]
mod playhead_tests;
#[cfg(test)]
mod rows_tests;
#[cfg(test)]
mod tools_tests;
// 输入框上方的列表、命令架与栈（K4）：移植自 Puffin 的壳，入参是中性的数据。
pub mod export_list;
pub mod lists;
pub mod model_picker;
pub mod modes;
pub mod queue_stack;
pub mod resume_list;
pub mod rewind_rows;
pub mod shelf;
pub mod theme_list;
pub mod when;
// 页与卡（设置、用量、目标、回退、门道）：移植自 Puffin 的壳，入参是中性的数据（tui-kit.md §4.4 K5）。
pub mod goal;
pub mod settings;
pub mod usage;
// 状态栏与 `/stats` 的卡（K4）。
pub mod stats;
pub mod status_bar;
// 请求、提问与计划的板与预览（K4）；数据从 Puffin 的投影搬来。
pub mod asks;
pub mod at_panel;
pub mod doorway;
#[cfg(test)]
mod goal_tests;
pub mod kept_rows;
#[cfg(test)]
mod list_rows_tests;
pub mod machine_rows;
pub mod mention_wire;
#[cfg(test)]
mod panel_rows_tests;
pub mod paste_caps;
pub mod skill_panel;
pub mod todo_dock;
