// 改写自 apps/zcode-host/crates/tui/src/ui/mouse.rs（feat/rs 79b6691c55）：去掉子代理、任务面板、后台任务、
// 背景标志的目标；点击网址不打开浏览器（tui 不起进程，docs/specs/rust-cli/tui.md §9）。
//! 鼠标（docs/specs/rust-tui.md §8.7、rust-cli/tui-select.md §2、§3）：每一帧绘制时记下可点击区域（[`Hits`]），
//! 鼠标事件只按上一帧的这份记录处理。transcript 的选区两端按（块的键，块内第几行，列）记录，追加新内容时不漂移；
//! 全屏视图（独立页面、详细视图、会话面板、`/usage`）的选区按屏幕坐标记（[`ScreenSelection`]）。选中的文字在绘制时
//! 算出，复制前先重画一次，保证拿到的是松开鼠标那一刻的选区。输入框的选区存在编辑区里（字节范围），这里只记拖动
//! 的起点；几种选区互斥。
//!
//! 缺陷修复（F25）：原来全屏视图里按下鼠标不建选区（`/status` 复制不了），松开时按终端决定要不要自动复制。现在全屏
//! 视图也能选，所有终端都松开即复制（用户决定）。
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use rust_cli_tui_kit::render::width::display_width;

use super::app::{Action, App, Overlay};
use super::composer::Popup;
use super::geom::Rect;
use super::selection::{text_at, word_at};

/// 连击（双击、三击）的间隔上限（原来 400 ms）。
const MULTI_CLICK: Duration = Duration::from_millis(500);
/// 连击允许的位置偏差（1 格）。
const MULTI_CLICK_CELLS: u16 = 1;
/// 连按两次选一个词，三次（及以后）选整行。
pub(crate) const DOUBLE_CLICK: u8 = 2;
pub(crate) const TRIPLE_CLICK: u8 = 3;
/// 选项的序号按十进制的一位数字键发出。
const DECIMAL: u32 = 10;

/// 可点击区域的目标。transcript 里可以展开的块不在这里：点击它们要和拖选共存，按 [`Hits::expandable`]
/// 在松开时处理（§8.2）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// 固定在第一行的提问：跳到这一行。
    Jump(usize),
    /// 回到底部控件。
    Bottom,
    /// 斜杠命令或文件列表的第 i 项。
    Popup(usize),
    /// 选择器的第 i 项（可见列表里的下标）。
    Item(usize),
    /// 审批或提问的第 i 个选项。
    Choice(usize),
    Shortcuts,
    /// 槽里驾驶舱的提示（tui-cockpit.md §3.4）：同按 ←。
    OpenCockpit,
    /// 任务行左边的一处（tui-tasks.md §2.2「点击」）：一个部分落在它的种类，记号与 `^T`（`None`）同按 ^T。
    Tasks(Option<super::task_card::TaskKind>),
    /// 任务卡这个种类的第 i 行（Subagents 0 = main，tui-tasks.md §3.2）。
    Task(usize),
    /// 驾驶舱的第 i 个代理：点它选中，点选中的等于 ⏎（tui-cockpit.md §3.1）。
    Cockpit(usize),
}

/// 上一帧的可点击区域与 transcript 的可见行。
#[derive(Default)]
pub struct Hits {
    pub regions: Vec<(Rect, Target)>,
    pub transcript: Rect,
    /// 可见行的（归属，纯文字）；下标 = 行 - `transcript.y`。
    pub rows: Vec<((u64, usize), String)>,
    /// 可见的、可以点击展开的块（块的键）。
    pub expandable: std::collections::HashSet<u64>,
    /// 当前选区的文字。
    pub selected: Option<String>,
    /// 这一帧画了的背景标志（tui.md §7.3）。
    pub mark: Option<super::logo::Stage>,
    /// 输入框文字区与它的第一可见行。
    pub composer: Option<(Rect, usize)>,
    /// 全屏视图这一帧的屏幕行（双击选词用）与软折行的续行（屏幕行号 → 记号，tui-select.md §2.3）。
    pub screen_rows: Vec<String>,
    pub soft: Vec<(u16, rust_cli_tui_kit::render::Continuation)>,
}

/// 选区的一端：（块的键，块内第几行，显示列）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Point {
    pub key: u64,
    pub offset: usize,
    pub col: usize,
}

/// 选区的单位（Puffin `Grain`，docs/puffin-tui.md §4.4「连击」）：单击是格，双击是词，三击是逻辑行。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grain {
    Cell,
    Word,
    Line,
}

/// transcript 的选区：`anchor` 是按下处、`head` 是指针处，选中的是两处各按 `grain` 取的单位的并集（同 Puffin 的
/// `base` 加指针下的单位）；单位在绘制时按排好的行算（`mouse_draw::select`），跟着内容走。格的选区两端相同时是空的：
/// 不画、不复制。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: Point,
    pub head: Point,
    pub grain: Grain,
    /// 左键还按着：拖动扩大选区。
    pub dragging: bool,
    /// 拖到 transcript 第一行（1）/ 最后一行（-1）时每帧滚动的方向。
    pub edge: isize,
    /// 这个选区复制过时复制出的字有多少（松开即复制之后；ctrl+c 只清掉它）。槽里的 `Copied N characters` 按它说
    /// （Puffin 的 `Copied.says` 在复制那一刻定下）：以前按每一帧重算的选中的字，流式回答在复制之后长出来的字也算进去，
    /// 字数变了、槽又淡入一次（孪生 copy-feedback）。
    pub copied: Option<Copied>,
}

/// 复制出的字的行数与字数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Copied {
    pub lines: usize,
    pub chars: usize,
}

impl Copied {
    pub fn of(text: &str) -> Copied {
        Copied {
            lines: text.split('\n').count(),
            chars: text.chars().count(),
        }
    }
}

/// 全屏视图里的选区：两端是屏幕坐标（列，行），含两端。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenSelection {
    pub anchor: (u16, u16),
    pub head: (u16, u16),
    pub dragging: bool,
    pub copied: bool,
}

/// 输入框里的一次拖选：按下时的范围（多击时是词或行）与连击次数。
#[derive(Debug, Clone, Copy)]
pub struct ComposerDrag {
    origin: (usize, usize),
    clicks: u8,
}

#[derive(Debug, Clone, Copy)]
pub struct Click {
    x: u16,
    y: u16,
    at: Instant,
    count: u8,
    /// 按在 transcript 上时按下处的位置：transcript 的连击比的是它（Puffin 比 transcript 的位置，不是屏幕格）。
    point: Option<Point>,
}

fn contains(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x && x < rect.right() && y >= rect.y && y < rect.bottom()
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

impl App {
    fn target_at(&self, x: u16, y: u16) -> Option<Target> {
        self.pointer
            .hits
            .regions
            .iter()
            .rev()
            .find(|(rect, _)| contains(*rect, x, y))
            .map(|(_, target)| target.clone())
    }

    /// 屏幕位置所在的、可以点击展开的块：位置在块的文字上（空白处不算），那里也没有别的可点击目标。
    fn block_at(&self, x: u16, y: u16) -> Option<u64> {
        let t = self.pointer.hits.transcript;
        if !contains(t, x, y) || self.target_at(x, y).is_some() {
            return None;
        }
        let ((key, _), text) = self.pointer.hits.rows.get(usize::from(y - t.y))?;
        (self.pointer.hits.expandable.contains(key) && text_at(text, usize::from(x - t.x)))
            .then_some(*key)
    }

    /// transcript 里屏幕位置对应的选区端点。
    fn point_at(&self, x: u16, y: u16) -> Option<Point> {
        let t = self.pointer.hits.transcript;
        let row = usize::from(y.checked_sub(t.y)?);
        let ((key, offset), _) = self.pointer.hits.rows.get(row)?;
        Some(Point {
            key: *key,
            offset: *offset,
            col: usize::from(x.saturating_sub(t.x)),
        })
    }

    pub fn on_mouse(&mut self, mouse: MouseEvent, now: Instant) -> Option<Action> {
        let (x, y) = (mouse.column, mouse.row);
        match mouse.kind {
            // 滚轮只有一个分发点（`wheel.rs`，tui-select.md §4.1）。
            MouseEventKind::ScrollUp => return self.wheel(false),
            MouseEventKind::ScrollDown => return self.wheel(true),
            _ => {}
        }
        if self.overlay.as_ref().is_some_and(Overlay::full_screen) {
            return self.on_screen_mouse(mouse, now);
        }
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => self.press_left(x, y, now),
            MouseEventKind::Drag(MouseButton::Left) => {
                self.drag(x, y);
                None
            }
            MouseEventKind::Up(MouseButton::Left) => self.release_left(x, y),
            MouseEventKind::Down(MouseButton::Right) => {
                self.right_click();
                None
            }
            MouseEventKind::Moved => {
                self.hover_moved(x, y);
                None
            }
            _ => None,
        }
    }

    /// 右键：Linux、Windows 上有选区时清掉（没有选区时不粘贴剪贴板，tui-select.md D5）；
    /// macOS 无作用。原来右键复制选区，现在不复制。
    fn right_click(&mut self) {
        if !cfg!(target_os = "macos") {
            self.pointer.selection = None;
            self.pointer.screen_selection = None;
        }
    }

    /// 全屏视图里的鼠标：点击目标照常，其余按屏幕坐标选择（tui-select.md §2.1）。
    fn on_screen_mouse(&mut self, mouse: MouseEvent, now: Instant) -> Option<Action> {
        let (x, y) = (mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(target) = self.target_at(x, y) {
                    self.pointer.press = Some(target);
                    return None;
                }
                let count = self.click_count(x, y, now);
                let row = self.pointer.hits.screen_rows.get(usize::from(y));
                let (from, to) = match (count, row) {
                    (DOUBLE_CLICK, Some(text)) => {
                        let (from, to) = word_at(text, usize::from(x));
                        (from, to.saturating_sub(1).max(from))
                    }
                    (TRIPLE_CLICK, Some(text)) => (
                        0,
                        text.chars()
                            .map(display_width)
                            .sum::<usize>()
                            .saturating_sub(1),
                    ),
                    _ => (usize::from(x), usize::from(x)),
                };
                let col = |c: usize| u16::try_from(c).unwrap_or(u16::MAX);
                self.pointer.screen_selection = Some(ScreenSelection {
                    anchor: (col(from), y),
                    head: (col(to), y),
                    dragging: count == 1,
                    copied: false,
                });
                None
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(selection) = self.pointer.screen_selection.as_mut()
                    && selection.dragging
                {
                    selection.head = (x, y);
                }
                None
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(target) = self.pointer.press.take() {
                    return (self.target_at(x, y) == Some(target.clone()))
                        .then(|| self.activate(target))
                        .flatten();
                }
                let selection = self.pointer.screen_selection.as_mut()?;
                if selection.dragging {
                    selection.dragging = false;
                    if selection.anchor == selection.head {
                        self.pointer.screen_selection = None;
                        return None;
                    }
                }
                Some(Action::CopySelection { clear: false })
            }
            MouseEventKind::Down(MouseButton::Right) => {
                self.right_click();
                None
            }
            MouseEventKind::Moved => {
                self.hover_moved(x, y);
                None
            }
            _ => None,
        }
    }

    /// 500 ms 内、离上次按下不超过 1 格的第几次按下（第 3 次及以后都是选整行）。
    fn click_count(&mut self, x: u16, y: u16, now: Instant) -> u8 {
        let near = |c: &Click| {
            c.x.abs_diff(x) <= MULTI_CLICK_CELLS && c.y.abs_diff(y) <= MULTI_CLICK_CELLS
        };
        let count = match self.pointer.click {
            Some(c) if near(&c) && now.duration_since(c.at) < MULTI_CLICK => {
                (c.count + 1).min(TRIPLE_CLICK)
            }
            _ => 1,
        };
        self.pointer.click = Some(Click {
            x,
            y,
            at: now,
            count,
            point: None,
        });
        count
    }

    /// transcript 上的第几次按下（Puffin `press`）：与上一次按在同一个 transcript 位置（块、块内行、列）且不到 500 ms
    /// 时多算一次，三次之后又是一次；只比两次按下的时刻。
    fn transcript_clicks(&mut self, x: u16, y: u16, point: Point, now: Instant) -> u8 {
        let count = match self.pointer.click {
            Some(c) if c.point == Some(point) && now.duration_since(c.at) < MULTI_CLICK => {
                c.count % TRIPLE_CLICK + 1
            }
            _ => 1,
        };
        self.pointer.click = Some(Click {
            x,
            y,
            at: now,
            count,
            point: Some(point),
        });
        count
    }

    /// 输入框文字区里屏幕位置对应的字节位置；拖出上下边时多走一行（输入框跟着光标滚动）。
    fn composer_offset(&self, x: u16, y: u16) -> Option<usize> {
        let (rect, first) = self.pointer.hits.composer?;
        let row = if y < rect.y {
            first.saturating_sub(1)
        } else if y >= rect.bottom() {
            first + usize::from(rect.height)
        } else {
            first + usize::from(y - rect.y)
        };
        let column = usize::from(x.saturating_sub(rect.x));
        Some(
            self.composer
                .area
                .offset_at(self.composer_width, row, column),
        )
    }

    fn press_left(&mut self, x: u16, y: u16, now: Instant) -> Option<Action> {
        if let Some(target) = self.target_at(x, y) {
            self.pointer.press = Some(target);
            return None;
        }
        // 背景标志：按下立刻按下去（tui.md §7.3）。开着的层的点击目标优先，所以放在上面的判断之后。
        if let Some(stage) = self.pointer.hits.mark
            && self.mark.press(&stage, x, y, now)
        {
            return None;
        }
        if let Some((rect, _)) = self.pointer.hits.composer
            && contains(rect, x, y)
        {
            self.pointer.selection = None;
            let clicks = self.click_count(x, y, now);
            let at = self.composer_offset(x, y)?;
            // 搜索历史时输入框显示的不是编辑区的文字：只放光标，不选择。
            let clicks = if self.composer.search.is_some() {
                1
            } else {
                clicks
            };
            let origin = self.composer.area.unit_range(at, clicks);
            self.composer.select(origin, at, clicks);
            self.pointer.composer_drag = Some(ComposerDrag { origin, clicks });
            return None;
        }
        if !contains(self.pointer.hits.transcript, x, y) {
            return None;
        }
        self.composer.area.clear_selection();
        self.pointer.screen_selection = None;
        let Some(point) = self.point_at(x, y) else {
            self.pointer.selection = None;
            self.pointer.click = None;
            return None;
        };
        // 挑选回退的点时按在你的一条消息（是点的）上是换点；按在别处照常开始选择（Puffin `rewind_key` 的点击）。
        if self.pick_at_row(point.key) {
            self.pointer.click = None;
            return None;
        }
        // 照 Puffin 的选择（docs/puffin-tui.md §4.4「选择与复制」）：按下时的选区就是按下处的单位——单击的格是空的（不画
        // 一格底色），双击是词，三击是逻辑行；拖动按同一单位扩大。以前单击就画一格，双击、三击只选屏幕上的一行且不能拖动。
        let grain = match self.transcript_clicks(x, y, point, now) {
            DOUBLE_CLICK => Grain::Word,
            TRIPLE_CLICK => Grain::Line,
            _ => Grain::Cell,
        };
        self.pointer.selection = Some(Selection {
            anchor: point,
            head: point,
            grain,
            dragging: true,
            edge: 0,
            copied: None,
        });
        None
    }

    fn drag(&mut self, x: u16, y: u16) {
        if let Some(drag) = self.pointer.composer_drag {
            if self.composer.search.is_none()
                && let Some(at) = self.composer_offset(x, y)
            {
                self.composer.select(drag.origin, at, drag.clicks);
            }
            return;
        }
        let t = self.pointer.hits.transcript;
        if t.height == 0 || !self.pointer.selection.is_some_and(|s| s.dragging) {
            return;
        }
        let row = y.clamp(t.y, t.bottom() - 1);
        let head = self.point_at(x, row);
        if let Some(selection) = self.pointer.selection.as_mut() {
            selection.edge = if y <= t.y {
                1
            } else if y >= t.bottom() - 1 {
                -1
            } else {
                0
            };
            if let Some(head) = head {
                selection.head = head;
            }
        }
    }

    fn release_left(&mut self, x: u16, y: u16) -> Option<Action> {
        // 输入框的选区松开时不自动复制（打字可以直接替换它）。
        if self.pointer.composer_drag.take().is_some() {
            return None;
        }
        if let Some(target) = self.pointer.press.take() {
            return (self.target_at(x, y) == Some(target.clone()))
                .then(|| self.activate(target))
                .flatten();
        }
        let selection = self.pointer.selection.as_mut()?;
        selection.edge = 0;
        selection.dragging = false;
        if selection.grain != Grain::Cell {
            // 双击、三击的选区（词与行从不是空的）：松开即复制。
            return Some(Action::CopySelection { clear: false });
        }
        if selection.anchor == selection.head {
            // 没有拖动：清掉选区；点在可以展开的块的文字上时展开 / 收起这个块（§8.2）。双击、三击的
            // 第二下起是选词、选行，不再切换。
            self.pointer.selection = None;
            if self.pointer.click.is_some_and(|c| c.count == 1)
                && let Some(key) = self.block_at(x, y)
                && !self.port.expanded_cells.remove(&key)
            {
                self.port.expanded_cells.insert(key);
            }
            return None;
        }
        // 松开即复制（tui-select.md §3.1）。
        Some(Action::CopySelection { clear: false })
    }

    /// 复制选区（`clear` 为真时复制后清掉选区）；transcript、全屏视图与输入框的选区都算。
    pub fn copy_selection(&mut self, clear: bool) -> Option<Action> {
        (self.pointer.selection.is_some()
            || self.pointer.screen_selection.is_some()
            || self.composer.area.selection().is_some())
        .then_some(Action::CopySelection { clear })
    }

    /// 有鼠标选区（transcript 或全屏视图，不在拖动中）。
    pub fn has_selection(&self) -> bool {
        self.pointer.selection.is_some_and(|s| !s.dragging)
            || self.pointer.screen_selection.is_some_and(|s| !s.dragging)
    }

    /// 复制过之后（松开即复制）记下；ctrl+c 据此只清选区。
    pub fn mark_copied(&mut self) {
        let copied = self.pointer.hits.selected.as_deref().map(Copied::of);
        if let Some(selection) = self.pointer.selection.as_mut() {
            selection.copied = copied;
        }
        if let Some(selection) = self.pointer.screen_selection.as_mut() {
            selection.copied = true;
        }
    }

    /// 点击目标：与对应的按键效果相同。
    fn activate(&mut self, target: Target) -> Option<Action> {
        let now = Instant::now();
        match target {
            Target::Jump(line) => {
                let top = (line + self.viewport).min(self.transcript_lines);
                self.port.scroll = self.transcript_lines - top;
                None
            }
            Target::Bottom => {
                self.port.scroll = 0;
                self.port.new_activity = false;
                None
            }
            Target::Popup(index) => {
                if let Some(Popup::Slash { selected } | Popup::Files { selected }) =
                    self.composer.popup.as_mut()
                {
                    *selected = index;
                }
                self.dispatch(key(KeyCode::Enter), now)
            }
            Target::Item(index) => {
                if let Some(Overlay::List(list)) = self.overlay.as_mut() {
                    list.selected = index;
                }
                self.dispatch(key(KeyCode::Enter), now)
            }
            Target::Choice(index) => {
                let digit = char::from_digit(u32::try_from(index + 1).ok()?, DECIMAL)?;
                self.dispatch(key(KeyCode::Char(digit)), now)
            }
            // 点 `? for shortcuts` 等于打出 `?`（草稿空着时，draft_layer.rs）。
            Target::Shortcuts => {
                if self.composer.is_empty() {
                    self.composer.set_text("?");
                    self.keys_closed = false;
                    self.overlay = Some(Overlay::Shortcuts);
                }
                None
            }
            // 槽只在草稿空着时有这个提示；点它同按 ←。
            Target::OpenCockpit => self.dispatch(key(KeyCode::Left), now),
            Target::Tasks(kind) => {
                self.click_tasks(kind);
                None
            }
            // 点击一行等于选中再按 enter。
            Target::Task(index) => self.tasks_activate(index),
            Target::Cockpit(index) => {
                let Some(Overlay::Sessions(list)) = self.overlay.as_mut() else {
                    return None;
                };
                if list.choice == index {
                    return self.dispatch(key(KeyCode::Enter), now);
                }
                list.choice = index;
                None
            }
        }
    }
}

#[path = "mouse_hover.rs"]
mod hover;
#[path = "mouse_draw.rs"]
mod paint;

pub use hover::Hover;

pub use paint::{draw_copied, paint_hover, paint_screen, select};
