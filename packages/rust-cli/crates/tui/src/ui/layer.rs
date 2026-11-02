//! 最上面的一层（架构整理 U7，用户决定 Q8，docs/specs/rust-cli/tui.md §7）：收按键、收粘贴与画在底部区域的是同一层，
//! 顺序是视图 → 待回答的交互 → 同伴消息的批准框 → 输入框。
//!
//! 缺陷修复：以前按键、粘贴、绘制各写一条判断链，顺序不同。按键先看视图，粘贴先看交互：异步的结果打开了带输入框的视图、
//! 下面的询问框正在写意见时，按键进视图，粘贴进了被盖住的询问框；询问框没在写意见时，粘贴进了看不见的输入框。快捷键
//! 面板开着时来了询问，画面上是询问框，第一个键却先关掉了看不见的面板。
use super::app::{App, Overlay};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    /// 打开着的视图（快捷键面板除外：它画在输入框上面，属于输入框这一层）。
    Overlay,
    /// 待回答的交互：当前会话的在前，之后是旧会话的询问（agents.md §4.1.2）。
    Interaction,
    /// 扣住的同伴消息的批准框（cross-session.md §2.6），没有待回答的交互时。
    Peer,
    /// 输入框，与跟它在一起的快捷键面板、子代理列表的焦点、搜索模式、只读查看、历史搜索。
    Composer,
}

impl App {
    /// 现在最上面的一层。
    pub fn top_layer(&self) -> Layer {
        if self.overlay.as_ref().is_some_and(Overlay::covers_input) {
            return Layer::Overlay;
        }
        if self.pending().is_some() {
            // 交互的选择状态在视图变化后才建好（`sync_interaction`）；建好之前按键与粘贴照旧给输入框这一层。
            return if self.interaction.is_some() {
                Layer::Interaction
            } else {
                Layer::Composer
            };
        }
        if self.view.peer_held().is_some() {
            return Layer::Peer;
        }
        Layer::Composer
    }
}

#[cfg(test)]
#[path = "layer_tests.rs"]
mod tests;
