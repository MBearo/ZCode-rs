//! 滚轮（修复线 F25，docs/specs/rust-cli/tui-select.md §4）：唯一的入口 [`App::wheel`] 按最上层的可滚动视图分发，
//! 没有时滚 transcript；每格滚几行是固定的（[`WheelLines`]）。
//!
//! 缺陷修复（F25）：原来滚轮按视图种类分散在 `on_mouse` 里，独立页面（`/help`、`/skills`、`/mcp`、`/status`、`/context`）、
//! `/usage`、`/permissions`、`/diff` 没有分支，滚轮落到被盖住的 transcript；会话面板的分支处理完又往下落，同时滚了
//! transcript。现在只有这一个分发点。
use std::collections::BTreeMap;

use super::app::{Action, App, Overlay};

/// 每格的行数（Puffin `WHEEL_LINES`）。
pub const WHEEL_LINES: usize = 3;
/// `ZCODE_SCROLL_SPEED` 的上限。
const MAX_SCROLL_SPEED: f64 = 20.0;

/// 每格滚几行：固定 [`WHEEL_LINES`]，`ZCODE_SCROLL_SPEED` 是大于 0 的数时用它（四舍五入，1 到 20；tui-select.md §4.2）。
/// 以前按终端与两格的间隔加速（窗口模式、滚轮模式、衰减曲线），照 Puffin 去掉（tui-kit.md §6「滚轮的行数」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WheelLines(usize);

impl Default for WheelLines {
    fn default() -> Self {
        Self(WHEEL_LINES)
    }
}

impl WheelLines {
    pub fn new(vars: &BTreeMap<String, String>) -> Self {
        vars.get(rust_cli_base::names::env::ZCODE_SCROLL_SPEED)
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|v| *v > 0.0)
            .map_or_else(Self::default, |v| {
                Self(v.min(MAX_SCROLL_SPEED).round().max(1.0) as usize)
            })
    }

    pub fn rows(self) -> usize {
        self.0
    }
}

impl App {
    /// 一格滚轮：按最上层的可滚动视图分发（tui-select.md §4.1）。列表类每格 1 项，滚动类按加速的行数。`/theme` 的列表
    /// 移了焦点时预览，交给 runner 重画（同按键，`slash_theme.rs` 的 `theme_preview`）。
    pub fn wheel(&mut self, down: bool) -> Option<Action> {
        // 设置页不随滚轮动（Puffin 设置开着时滚轮进 `settings_key`，它不收）：页的行随选择滚到看得见。
        if self.settings.is_some() {
            return None;
        }
        let step: isize = if down { 1 } else { -1 };
        let rows = self.pointer.wheel.rows() as isize;
        let lines = if down { rows } else { -rows };
        // 全屏视图的选区按屏幕坐标记：内容滚动之后不再对应原来的文字（tui-select.md D3）。
        if self.overlay.as_ref().is_some_and(Overlay::full_screen) {
            self.pointer.screen_selection = None;
        }
        // 驾驶舱：滚轮选（tui-cockpit.md §3.1）。
        if let Some(Overlay::Sessions(list)) = self.overlay.as_mut()
            && list.cockpit.is_some()
            && list.step == super::sessions_steps::Step::List
        {
            let n = list.matches().len() as isize;
            if n > 0 {
                list.choice = (list.choice as isize + step).rem_euclid(n) as usize;
            }
            return None;
        }
        // 统计的页：滚显示的标签，Context 不动选中（stats_page.rs）。
        if self.stats_page_wheel(lines) {
            return None;
        }
        match &self.overlay {
            Some(Overlay::Usage(_)) => {
                let max_top = self.page_rows.1.saturating_sub(self.page_rows.0);
                if let Some(Overlay::Usage(view)) = self.overlay.as_mut() {
                    view.wheel(lines, max_top);
                }
            }
            Some(Overlay::Diff(_)) => self.diff_wheel(lines),
            Some(Overlay::List(_)) => {
                if let Some(Overlay::List(list)) = self.overlay.as_mut() {
                    list.wheel(step);
                }
                return self.theme_preview();
            }
            Some(Overlay::Trust(_)) => {}
            Some(Overlay::Tasks(_)) => self.move_task(step),
            Some(
                Overlay::Shortcuts
                | Overlay::YoloConfirm(_)
                | Overlay::Machines(_)
                | Overlay::Card(_)
                | Overlay::Sessions(_)
                | Overlay::Points(_)
                | Overlay::Forks(_),
            )
            | None => {
                // ⌃t 读的页开着时滚它（sessions_steps.rs）。
                if self.sessions_page_wheel(lines) {
                    return None;
                }
                self.scroll_shown(-lines);
            }
        }
        None
    }
}

#[cfg(test)]
#[path = "wheel_tests.rs"]
mod tests;
