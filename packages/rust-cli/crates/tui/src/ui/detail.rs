//! ctrl+o（docs/puffin-tui.md §4.2「^O」，Puffin `details_open`）：transcript 在原地换成详细的样子——一段工作展开成每个调用、
//! 思考显示全文、长的消息不剪——再按一次换回来。滚动、选择与搜索就是主界面的。
//!
//! 以前 ctrl+o 打开占满屏幕的详细视图（`Transcript  ›  Details` 的标题、自己的滚动与 `/` 搜索、底行的提示），与 Puffin
//! 不同（tui-kit.md §6 的那一行随之删除）；它的滚动与搜索主界面都有（tui-input.md §2.3）。
use super::app::App;
use super::i18n::Lang;

impl App {
    /// ctrl+o：详细的样子与平常的样子互换。换回平常的样子时全部收起（docs/puffin-tui.md §4.2「工作的收拢」的「展开」）。
    ///
    /// 缺陷修复：以前只翻 `details_open`，点击（与搜索的 enter）记下的单独展开留着；详细视图里块不能点，看不出它们
    /// 还记着，^O 关上后这些块仍是展开的样子，看起来 ^O 收不起来。现在每次切换都清掉单独展开：详细视图里本来不看
    /// 它，关上时就是全部收起。
    pub(super) fn open_detail(&mut self) {
        self.details_open = !self.details_open;
        self.port.expanded_cells.clear();
    }
}

/// 没有匹配（主界面的搜索模式，tui-input.md §2.3.5）。
pub(super) fn no_matches(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "no matches",
        Lang::Zh => "没有匹配",
    }
}
