//! `App` 的子状态（架构整理 U3）：一起变的字段放在一个结构里，重置写成 `Default`。
//!
//! 以前这些字段平铺在 `App` 的 64 个字段里，离开会话（`App::reset`）与换显示的 transcript（开始、换一个、结束查看
//! 子代理）各有一份手写的重置列表，后一份漏了全屏视图的选区（`screen_selection`）。现在两处都调
//! [`App::shown_changed`](super::app::App::shown_changed)：读位置整体换成 `Port::default()`，两种选区一起清掉。
use std::collections::HashSet;
use std::time::Instant;

use super::mouse::{Click, ComposerDrag, Hits, ScreenSelection, Selection, Target};

/// 显示的 transcript 里的读位置：显示的 transcript 换了（离开会话、开始或结束查看子代理）时整体回到 `Default`。
#[derive(Default)]
pub struct Port {
    /// 从底部向上滚过的行数；0 表示跟随最新内容。
    pub scroll: usize,
    /// 上滚期间又来了新内容。
    pub new_activity: bool,
    /// 点击展开行单独展开的 cell（第一个行 id）。
    pub expanded_cells: HashSet<u64>,
    /// 主界面的搜索模式（`//`，修复线 F114，tui-input.md §2.3）。
    pub find: Option<super::find::Find>,
    /// 下一帧把这一行（块的键，块内行）放回视口的最后一行：退出搜索模式时回到进入前的位置。
    pub restore_bottom: Option<(u64, usize)>,
}

/// 鼠标与选区：上一帧的可点击区域、悬停、拖选、复制的提示与滚轮的加速。
#[derive(Default)]
pub struct Pointer {
    /// 悬停：鼠标最后移到的位置与那时那里的东西（tui-select.md §9）。
    pub hover: Option<super::mouse::Hover>,
    /// 上一帧的可点击区域与可见行。
    pub hits: Hits,
    pub selection: Option<Selection>,
    pub(crate) composer_drag: Option<ComposerDrag>,
    pub(crate) click: Option<Click>,
    pub(crate) press: Option<Target>,
    /// 全屏视图里的选区（屏幕坐标，tui-select.md §2.1）；与 `selection` 互斥。
    pub screen_selection: Option<ScreenSelection>,
    /// 最近一次复制的提示与时间（gap 行右侧显示，时长按途径）。
    pub copied: Option<(super::clipboard::Notice, Instant)>,
    /// 一格滚轮滚几行（tui-select.md §4.2）；runner 按终端变量换成对应的行数。
    pub wheel: super::wheel::WheelLines,
}

impl Pointer {
    /// 清掉 transcript 与全屏视图的选区。
    pub fn clear_selection(&mut self) {
        self.selection = None;
        self.screen_selection = None;
    }
}

/// 按键的状态：入口的归一与 paste burst、和弦、历史浏览的提示、终端分不分得出增强按键。
#[derive(Default)]
pub struct KeyState {
    /// 按键的入口（松开事件与 alt 码、AltGr 的归一）与没有括号粘贴时的 paste burst（只在 Windows 上有，tui-input.md §9，
    /// 修复线 F65）；runner 按入口注入的平台设置（[`App::set_os`](super::app::App::set_os)）。
    pub keyboard: super::keyboard::Keyboard,
    pub burst: Option<super::paste_burst::PasteBurst>,
    /// ctrl+x 和弦的第一个键按下的时间（tui-input.md §5）。
    pub chord: Option<Instant>,
    pub history_presses: usize,
    pub history_hinted: bool,
    /// 终端分辨得出增强按键（修复线 F104，tui-input.md §11.1.1）：进界面时键盘增强的查询结论，由 `Screen` 带进来；
    /// 决定立即发送的提示写 ctrl+enter 还是 ctrl+x ctrl+s。
    pub keyboard_enhanced: bool,
}

/// 当前会话的快照（runner 按请求序号写入）：离开会话时清空。
#[derive(Default)]
pub struct Snapshots {
    /// 任务清单的界面状态（ctrl+t，tui-display.md §2.2）。
    pub tasks: super::tasks::TaskPanel,
    /// 后台任务的最新快照（与 `agents` 同一份快照、同一个回执，tui-display.md §2.3，修复线 F34）。
    pub background: Vec<rust_cli_kernel_api::manage::BackgroundEntry>,
    /// `/stats` 卡的请求统计（缓存命中率与速度）的最后一份回执（tui-display.md §2.6，F27）。
    pub request_stats: super::footer_stats::FooterStats,
    /// 子代理的最后一份快照（与 `background` 同一份快照，tui-panels.md §0、§1）。
    pub agents: super::agents::AgentList,
}

impl Snapshots {
    /// 离开会话：清空；请求统计的序号照旧往前走，在途的回执作废（F27）。
    pub fn clear(&mut self) {
        let mut request_stats = std::mem::take(&mut self.request_stats);
        request_stats.clear();
        *self = Self {
            request_stats,
            ..Self::default()
        };
    }
}

#[cfg(test)]
#[path = "app_state_tests.rs"]
mod tests;
