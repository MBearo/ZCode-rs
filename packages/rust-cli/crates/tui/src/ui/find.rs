//! 主界面的搜索模式（docs/specs/rust-cli/tui-input.md §2.3，修复线 F114）：空输入框里手打 `//` 进入（查看子代理时
//! `/`），边输边搜当前显示的 transcript（含折叠的块），↑ ↓ 换当前匹配，enter 停下，esc 回到进入前的位置。
//!
//! 全是界面状态：当前匹配按（块的键，块内第几个匹配）记，不按屏幕行记；每一帧按当前宽度重新排版、重新找，再按这个身份
//! 找回当前匹配（流式追加、改宽度都不丢）。不用超时。
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::app::{Action, App};
use super::find_text::{fold, pop_grapheme};
use super::i18n::Lang;
use super::transcript::{Layout, Target};

/// 下一帧怎么定当前匹配：`nearest` 先取离参照点最近的（查询变了），再按 `step` 往前 / 往后挪（↑ ↓，回绕）。两者都没有
/// 时找回上一帧的当前匹配（被动的帧：流式追加、改宽度）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Pick {
    pub nearest: bool,
    pub step: isize,
}

/// 当前匹配：（块的键，块内第几个匹配），与上一帧它在块的完整形式里的行和列（取最近的匹配时作参照）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub key: u64,
    pub ordinal: usize,
    pub line: usize,
    pub col: usize,
    /// 这一块原来折叠着，只为显示当前匹配临时展开：enter 停在这里时记进单独展开。
    pub peeked: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Find {
    pub query: String,
    pub current: Option<Hit>,
    pub pick: Pick,
    /// 进入时的位置：None 是跟随末尾；否则是视口最后一行的归属（块的键，块内行）。
    pub origin: Option<(u64, usize)>,
    /// 上一帧的匹配数与当前匹配的序号（输入框右侧的 `3/12`）。
    pub count: usize,
    pub index: Option<usize>,
    /// 上一帧的宽度：变了时把当前匹配滚进视口。
    width: usize,
    /// 当前匹配与 origin 记下时排版缓存的 epoch：视图重建（行 id 从头分配）之后两者作废。
    epoch: Option<u64>,
    /// 查看子代理时单个 `/` 进入：紧跟的一个 `/` 丢掉（手打 `//` 与主会话的结果相同）。
    swallow_slash: bool,
}

/// 这一帧的搜索结果（绘制用）：转过小写的查询、当前匹配的位置（块的键，块内行，起止列），要不要把它滚进视口，
/// 要不要回到进入前的位置（有查询的输入之后没有匹配）。
pub(super) struct Settled {
    needle: String,
    current: Option<(u64, usize, usize, usize)>,
    follow: bool,
    snap_back: bool,
}

fn pick(lang: Lang, en: &'static str, zh: &'static str) -> &'static str {
    match lang {
        Lang::En => en,
        Lang::Zh => zh,
    }
}

/// 查询为空时的占位文字。
pub fn placeholder(lang: Lang) -> &'static str {
    pick(lang, "Search the transcript", "搜索对话记录")
}

/// footer 左侧的按键提示（按键提示组件的模板）。
pub fn hint(lang: Lang) -> &'static str {
    pick(
        lang,
        "{↑↓} previous/next · {enter} stop here · {esc} exit",
        "{↑↓} 上一个/下一个 · {enter} 停在这里 · {esc} 退出",
    )
}

impl App {
    /// 进入搜索模式；`swallow_slash`：查看子代理时单个 `/` 进入，紧跟的一个 `/` 丢掉。进入时的位置取上一帧视口的最后
    /// 一行（跟随末尾时不记）。
    pub(super) fn open_find(&mut self, swallow_slash: bool) {
        let origin = (self.scroll > 0)
            .then(|| self.hits.rows.last().map(|(owner, _)| *owner))
            .flatten();
        self.find = Some(Find {
            query: String::new(),
            current: None,
            pick: Pick::default(),
            origin,
            count: 0,
            index: None,
            width: 0,
            epoch: None,
            swallow_slash,
        });
    }

    /// 退出搜索模式。`stay` 且有当前匹配时停在那里（临时展开的块记进单独展开）；否则回到进入前的位置。
    fn close_find(&mut self, stay: bool) {
        let Some(find) = self.find.take() else {
            return;
        };
        match find.current.filter(|_| stay) {
            Some(hit) => {
                if hit.peeked {
                    self.expanded_cells.insert(hit.key);
                }
            }
            None => self.back_to(find.origin),
        }
    }

    /// 回到 `origin`：跟随末尾时马上回到底部；否则下一帧把那一行放回视口的最后一行（要按那一帧的排版换算）。
    fn back_to(&mut self, origin: Option<(u64, usize)>) {
        match origin {
            Some(anchor) => self.restore_bottom = Some(anchor),
            None => {
                self.scroll = 0;
                self.new_activity = false;
            }
        }
    }

    /// 搜索模式的按键（tui-input.md §2.3.1）。
    pub(super) fn find_key(&mut self, key: KeyEvent) -> Option<Action> {
        let ctrl = self.typing.ctrl(&key);
        let typed = self.typing.is_plain_text(&key);
        let page = self.viewport.saturating_sub(1).max(1) as isize;
        let find = self.find.as_mut()?;
        let swallow = std::mem::take(&mut find.swallow_slash);
        match key.code {
            KeyCode::Up => find.step(-1),
            KeyCode::Down => find.step(1),
            KeyCode::Char('p') if ctrl => find.step(-1),
            KeyCode::Char('n') if ctrl => find.step(1),
            KeyCode::Backspace if !find.query.is_empty() => {
                pop_grapheme(&mut find.query);
                find.pick.nearest = true;
            }
            KeyCode::Char('/') if swallow && find.query.is_empty() => {}
            KeyCode::Char(c) if typed => {
                find.query.push(c);
                find.pick.nearest = true;
            }
            // 查询已经空了时的 backspace 同 esc。
            KeyCode::Esc | KeyCode::Backspace => self.close_find(false),
            KeyCode::Char('c') if ctrl => self.close_find(false),
            KeyCode::Char('o') if ctrl => {
                self.close_find(false);
                self.open_detail();
            }
            KeyCode::Enter => self.close_find(true),
            KeyCode::PageUp => return self.scroll_by(page),
            KeyCode::PageDown => return self.scroll_by(-page),
            _ => {}
        }
        None
    }

    /// 搜索模式里的粘贴：加进查询，换行压成空格。
    pub(super) fn find_paste(&mut self, text: &str) {
        if let Some(find) = self.find.as_mut() {
            find.swallow_slash = false;
            let text = text.replace("\r\n", " ").replace(['\n', '\r'], " ");
            find.query.push_str(&text);
            find.pick.nearest = true;
        }
    }
}

impl Find {
    fn step(&mut self, by: isize) {
        if self.count > 0 {
            self.pick.step += by;
        }
    }
}

/// 当前匹配所在的块（排版时按展开的样子排）。
pub(super) fn peek(app: &App) -> Option<u64> {
    app.find.as_ref()?.current.map(|hit| hit.key)
}

/// 每一帧：在排好的 transcript 里找，按 `pick` 定当前匹配，记下匹配数与序号。`epoch` 是排版缓存的 epoch，`width` 是这一帧
/// 的宽度。当前匹配换了块时调用方要再排一次（[`peek`] 变了）。
pub(super) fn settle(app: &mut App, layout: &Layout, epoch: u64, width: usize) -> Option<Settled> {
    let App {
        find,
        expanded_cells,
        ..
    } = app;
    let find = find.as_mut()?;
    if find.epoch != Some(epoch) {
        // 视图重建过：行 id 从头分配，记下的块的键不再作数。
        if find.epoch.is_some() {
            find.current = None;
            find.origin = None;
        }
        find.epoch = Some(epoch);
    }
    let needle = fold(&find.query);
    let targets = layout.targets(&needle);
    let resized = std::mem::replace(&mut find.width, width) != width;
    let pick = std::mem::take(&mut find.pick);
    let index = (!targets.is_empty()).then(|| {
        let found = find.current.and_then(|hit| {
            targets
                .iter()
                .position(|t| t.key == hit.key && t.ordinal == hit.ordinal)
        });
        let base = match found {
            Some(at) if !pick.nearest => at,
            // 查询变了、或者找不回来（块不见了、匹配变少了）：取离参照点最近、不在它后面的。
            _ => nearest(&targets, reference(find, layout)),
        };
        (base as isize + pick.step).rem_euclid(targets.len() as isize) as usize
    });
    find.count = targets.len();
    find.index = index;
    let current = index.map(|at| targets[at]);
    find.current = current.map(|t| Hit {
        key: t.key,
        ordinal: t.ordinal,
        line: t.line,
        col: t.from,
        peeked: t.expandable && !expanded_cells.contains(&t.key),
    });
    let moved = pick.nearest || pick.step != 0;
    Some(Settled {
        needle,
        current: current.map(|t| (t.key, t.line, t.from, t.to)),
        follow: moved || resized,
        snap_back: pick.nearest && targets.is_empty(),
    })
}

/// 取最近的匹配用的参照点（块的序号，块内行，列）：当前匹配原来的位置；没有时是进入时的位置（视口最后一行）；跟随末尾
/// 或那一块不在了时为 None（末尾）。
fn reference(find: &Find, layout: &Layout) -> Option<(usize, usize, usize)> {
    if let Some(hit) = find.current
        && let Some(block) = layout.block_index(hit.key)
    {
        return Some((block, hit.line, hit.col));
    }
    let (key, line) = find.origin?;
    Some((layout.block_index(key)?, line, usize::MAX))
}

/// 不在参照点后面的最后一个匹配；没有就取第一个。参照点为 None（末尾）时是最后一个。
fn nearest(targets: &[Target], reference: Option<(usize, usize, usize)>) -> usize {
    match reference {
        None => targets.len() - 1,
        Some(at) => targets
            .iter()
            .rposition(|t| (t.block, t.line, t.from) <= at)
            .unwrap_or(0),
    }
}

/// 绘制里排版之后：esc 之后回到进入前的位置；搜索中当前匹配换了或宽度变了时，它不在视口里就滚到中间；有查询的输入之后
/// 没有匹配时回到进入前的位置。`height` 是 transcript 的高度。
pub(super) fn scroll(app: &mut App, layout: &Layout, settled: Option<&Settled>, height: usize) {
    if let Some(anchor) = app.restore_bottom.take() {
        put_bottom(app, layout, Some(anchor), height);
    }
    let Some(settled) = settled else {
        return;
    };
    if settled.snap_back {
        let origin = app.find.as_ref().and_then(|find| find.origin);
        put_bottom(app, layout, origin, height);
        return;
    }
    let (Some((key, line, _, _)), true) = (settled.current, settled.follow) else {
        return;
    };
    let Some(at) = layout.at(key, line) else {
        return;
    };
    let total = layout.len();
    let end = total.saturating_sub(app.scroll);
    if (end.saturating_sub(height)..end).contains(&at) {
        return;
    }
    let top = at.saturating_sub(height / 2);
    app.scroll = total
        .saturating_sub(top + height)
        .min(total.saturating_sub(height));
    if app.scroll == 0 {
        app.new_activity = false;
    }
}

/// 把（块的键，块内行）放回视口的最后一行；None（或那一块不在了）时回到底部。
fn put_bottom(app: &mut App, layout: &Layout, anchor: Option<(u64, usize)>, height: usize) {
    let total = layout.len();
    app.scroll = anchor
        .and_then(|(key, line)| layout.at(key, line))
        .map_or(0, |at| {
            total
                .saturating_sub(at + 1)
                .min(total.saturating_sub(height))
        });
    if app.scroll == 0 {
        app.new_activity = false;
    }
}

/// 可见行（从第 `start` 行起）上的高亮：每一处出现画 `find_match`，当前匹配画 `find_current`。
pub(super) fn paint(
    app: &App,
    layout: &Layout,
    visible: &mut [Line<'static>],
    start: usize,
    settled: &Settled,
) {
    let range = start..start + visible.len();
    let matched = app.palette.find_match();
    for (line, from, to) in layout.find_in(&settled.needle, range.clone()) {
        super::selection::paint(&mut visible[line - start], from, to, matched);
    }
    if let Some((key, line, from, to)) = settled.current
        && let Some(at) = layout.at(key, line).filter(|at| range.contains(at))
    {
        super::selection::paint(
            &mut visible[at - start],
            from,
            to,
            app.palette.find_current(),
        );
    }
}

/// 搜索模式的输入框一行（tui-input.md §2.3.5）与光标所在的列：`⌕ ` 加查询，右侧是 `序号/总数` 或 `no matches`；
/// 放不下时先省右侧，查询太长时只显示末尾。
pub fn input_line(app: &App, find: &Find, width: usize) -> (Line<'static>, u16) {
    /// 提示符 `⌕ ` 占 2 列；查询与右侧的序号之间至少空 2 列（其中一列留给光标）。
    const PROMPT_WIDTH: usize = 2;
    const STATUS_GAP: usize = 2;
    let prompt = Span::styled("⌕ ", Style::default().fg(app.palette.accent));
    if find.query.is_empty() {
        let dim = Style::default().add_modifier(Modifier::DIM);
        let line = Line::from(vec![prompt, Span::styled(placeholder(app.lang), dim)]);
        return (super::text::truncate_line(line, width), PROMPT_WIDTH as u16);
    }
    // 留 1 列给行尾的光标。
    let room = width.saturating_sub(PROMPT_WIDTH + 1);
    let shown = tail(&find.query, room);
    let used = PROMPT_WIDTH + shown.width();
    let status = match find.index {
        Some(index) => format!("{}/{}", index + 1, find.count),
        None => super::detail::no_matches(app.lang).to_owned(),
    };
    let mut spans = vec![prompt, Span::raw(shown)];
    if used + STATUS_GAP + status.width() <= width {
        spans.push(Span::raw(" ".repeat(width - used - status.width() - 1)));
        spans.push(Span::styled(status, app.palette.secondary()));
    }
    (Line::from(spans), used.min(width.saturating_sub(1)) as u16)
}

/// `text` 的末尾不超过 `room` 列的那一段（按字素截）。
fn tail(text: &str, room: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    if text.width() <= room {
        return text.to_owned();
    }
    let mut used = 0;
    let mut start = text.len();
    for (at, grapheme) in text.grapheme_indices(true).rev() {
        used += grapheme.width();
        if used > room {
            break;
        }
        start = at;
    }
    text[start..].to_owned()
}

#[cfg(test)]
#[path = "find_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "find_view_tests.rs"]
mod view_tests;
