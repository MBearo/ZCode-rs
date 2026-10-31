//! 跨面板的排版常量（docs/specs/rust-cli/constants.md §5.3）：几个面板、对话框与行共用的缩进、留白与最少行数。
//! 只在一个文件里用的宽度与行数留在那个文件顶部。
//!
//! 单位：宽度是终端列数（`usize`），高度是行数。

/// 面板正文与按键提示行的缩进（两个空格）。
pub const INDENT: usize = 2;
/// 面板底部给按键提示留的行：一个空行、一行提示。
pub const FOOTER_ROWS: usize = 2;
/// 面板的列表放不下时至少显示的条目行数。
pub const MIN_LIST_ROWS: usize = 3;
/// 一行右端留的空白，免得紧贴终端的右边。
pub const RIGHT_MARGIN: usize = 2;
/// 画框（`╭─╮`、`┌─┐`）里正文左右各一列的留白，合计两列。
pub const BOX_PADDING: usize = 2;
/// 画框左右两条边线，合计两列。
pub const BOX_BORDERS: usize = 2;
/// 还不知道终端宽度（没有画过）时按这么宽排版。
pub const DEFAULT_WIDTH: usize = 80;
