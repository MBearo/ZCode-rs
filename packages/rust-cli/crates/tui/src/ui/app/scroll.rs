//! 正文的滚动（tui-kit.md §4.0「分隔行」）：离末尾的偏移 `scroll`，0 是跟随末尾；从上一帧画出的顶行算，移到跟随的
//! 顶行或更下就跟随（Puffin `scroll`、`scroll_to`）。从 `app.rs` 原样移来。
use super::{Action, App};

impl App {
    /// 跟随的帧的正文行数换成 `rows`：锚定时顶行不动（Puffin 的锚记着行，不记离末尾多远），偏移跟着改。还没画过（`viewport`
    /// 是 0）时偏移是调用方直接给的，不改。
    pub fn set_viewport(&mut self, rows: usize) {
        if self.port.scroll > 0 && self.viewport > 0 {
            let moved = self.port.scroll as isize + self.viewport as isize - rows as isize;
            self.port.scroll = moved.max(1) as usize;
        }
        self.viewport = rows;
    }

    /// 内容变长时保持用户正在看的位置（已上滚时）。
    pub fn content_resized(&mut self, lines: usize) {
        if self.port.scroll > 0 && lines > self.transcript_lines {
            self.port.scroll += lines - self.transcript_lines;
            self.port.new_activity = true;
        }
        self.transcript_lines = lines;
        // 锚定的视图在内容放得下时也留着（偏移 1，顶行仍是 0），见 `scroll_by`。
        self.port.scroll = self
            .port
            .scroll
            .min(lines.saturating_sub(self.viewport).max(1));
        if self.port.scroll == 0 {
            self.port.new_activity = false;
        }
    }

    /// 正文从上一帧画出的顶行往上移 `lines` 行（负数往下），到了跟随的顶行或更下就跟随（Puffin `scroll_to`）。
    /// 修复：以前按离末尾的偏移移，跟随时置顶行占的那一行没算进去，翻一页比 Puffin 多走一行。
    pub fn scroll_shown(&mut self, lines: isize) -> Option<Action> {
        // 跟随的顶行不小于 0：内容放得下时往上滚的目标在它上面，要锚定（`scroll_by`）。
        let follow = self.transcript_lines.saturating_sub(self.viewport) as isize;
        let at = follow - self.shown_top as isize;
        self.scroll_by(at - self.port.scroll as isize + lines)
    }

    /// ctrl+Home：滚到第 0 行。Puffin 的目标行是 0，它不在跟随的顶行上面（放得下时）就跟随，所以按到顶的偏移移，
    /// 不按一个很大的数。
    pub fn scroll_home(&mut self) -> Option<Action> {
        let top = self.transcript_lines.saturating_sub(self.viewport) as isize;
        self.scroll_by(top - self.port.scroll as isize)
    }

    /// 翻一页的行数：跟随末尾时正文的行数减 1（Puffin `body_height − 1`）。以前是含置顶行与分隔行的高度减 1。
    pub fn page_rows(&self) -> isize {
        self.follow_rows.saturating_sub(1).max(1) as isize
    }

    pub fn scroll_by(&mut self, lines: isize) -> Option<Action> {
        // 滚动结束请求停在它的行上：从这里起视图是你的（Puffin `scroll_to`）。
        self.unpin_ask();
        // 挑选回退的点时滚动过就不再停在切线上（滚到底停在最后一屏，Puffin `scroll_to`）。
        if let Some(picking) = self.picking.as_mut() {
            picking.at_cut = false;
        }
        // 目标在跟随的顶行上面（离末尾的偏移 > 0）就锚定，内容放得下时也是（Puffin `scroll_to`：`line < end.top`）。
        // 修复：以前偏移夹到 `lines − viewport`，放得下时是 0，往上滚丢了；Puffin 停在第 0 行，之后的内容在下面长出
        // 「↓ N more」，旧界面一直跟到底（孪生场景 scroll-separator）。放得下时偏移记 1，顶行仍是 0。
        let max = self.transcript_lines.saturating_sub(self.viewport);
        let want = self.port.scroll as isize + lines;
        let next = if want > 0 {
            (want as usize).min(max.max(1))
        } else {
            0
        };
        self.port.scroll = next;
        if next == 0 {
            self.port.new_activity = false;
        }
        None
    }
}
