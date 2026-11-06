//! 有状态的渲染核心：每帧在备用屏里画满整个终端（docs/puffin-tui.md §4.4；docs/specs/rust-cli/tui-kit.md §3.2）。
//! 移植自 packages/tui/tui/src/zcode/tui/render/Renderer.scala（feat/tui a2bf878b34）。
//!
//! 第一帧进入备用屏（`?1049h`）并画出每个格子；之后同尺寸的帧把打包的格子网与上一帧逐格比较，只发出最少的光标移动与
//! 合并了样式的文字；尺寸变化（或 [`Renderer::invalidate`]）清屏重画。图形画在文字之后，只在变化或它的格子被重写时重画。
//! 最后把隐藏的终端光标放到这一帧的光标格（[`Surface::caret`]）。每帧是一次写，包在 DEC 2026 同步输出里，用户看不到画了
//! 一半的帧。
//!
//! 与 Scala 的不同：Scala 把帧写进构造时给的 `out`；这里 [`Renderer::render`] 返回这一帧的字节（no-op 快路径返回
//! `None`），调用方写出——"一帧一次写"由构造保证。同样的输入写出的字节与 Scala 逐字节相同（画面哈希与 VT 解码器依赖
//! 这种方言）。图形的位置是 `usize`：Scala 的 `Int` 只在 `dirtied` 里按 0 截断，调用方从不给负数。
//!
//! 颜色深度（docs/specs/rust-cli/tui-kit.md §3.4，Scala 没有）：[`Renderer::with_depth`] 给样式池一个深度，格子的样式与
//! 涂的地都按它降采样输出；缺省是真彩，字节与 Scala 相同。
use std::fmt::Write as _;

use super::ansi;
use super::cell::{Cell, WidthClass};
use super::color::{self, Color, ColorDepth};
use super::line::StyledLine;
use super::pool::StylePool;
use super::surface::Surface;
use super::width;

/// 一帧文字之后画的图片：在屏幕 (`row`, `col`)（0 起）处占 `rows` × `cols` 个格子。`draw` 是光标在它左上格时画出它的
/// 字节；`erase` 是去掉它的字节（kitty 按 id 删；sixel 与 iTerm2 的图片不用，写在它格子上的文字就盖掉它）。`id` 跨帧
/// 标识它。按值比较（与 Scala 的 case class 相同）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Graphic {
    pub id: u32,
    pub row: usize,
    pub col: usize,
    pub rows: usize,
    pub cols: usize,
    pub draw: String,
    pub erase: String,
}

/// 主题涂的地（docs/puffin-tui.md §4.2「谁涂地」）：没有颜色的格子写成 `bg` 上的 `fg`，擦除取 `bg`，终端的默认色也设成
/// 它们（OSC 10/11），终端的边距就对上了。离开备用屏时把 `restore`（第一次探测到的终端自己的颜色）还给终端；`None` 时
/// 请终端回到它的缺省（OSC 110/111）：外观变过之后，探测到的颜色已经过时。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Backdrop {
    pub fg: Color,
    pub bg: Color,
    pub restore: Option<(Color, Color)>,
}

impl Backdrop {
    pub fn colors(&self) -> (Color, Color) {
        (self.fg, self.bg)
    }

    /// 在这个地之后把终端自己的默认色还给它的字节。
    fn give_back(&self) -> String {
        match self.restore {
            Some((fg, bg)) => ansi::set_colors(fg, bg),
            None => ansi::RESET_COLORS.to_string(),
        }
    }
}

/// 见模块文档。不是线程安全的：只由运行时的帧步骤调用。
pub struct Renderer {
    pool: StylePool,
    alt_screen_setup: String,
    alt_screen_teardown: String,
    active: bool,
    prev: Surface,
    // 上一帧的（宽，高）；`None` 是没有基线（还没画过，或 invalidate 之后），下一帧整屏重画。
    prev_dims: Option<(usize, usize)>,
    prev_graphics: Vec<Graphic>,
    // 终端现在的地：进入备用屏或它变化时设，离开时还回去。不在备用屏时是 `None`。
    painted: Option<Backdrop>,
    // 这一帧整行重写过的行（宽度有争议的字形）；这些行上的图形要重画。
    rewritten: Vec<bool>,
    // 上一帧的长度：下一帧的缓冲一次分配到这么大，重的画面不必每帧重新长大（Scala 复用一个 StringBuilder）。
    frame_hint: usize,
}

impl Renderer {
    /// `alt_screen_setup` 紧跟在 `?1049h` 之后写，`alt_screen_teardown` 紧挨在 `?1049l` 之前写。来自终端的备用屏设置
    /// （能力够的 TTY 上是 kitty 键盘协议的 push / pop，否则为空），因为那个增强是按屏幕缓冲记的。空串让无头与测试的字节流
    /// 保持不变。
    pub fn new(alt_screen_setup: String, alt_screen_teardown: String) -> Renderer {
        Renderer {
            pool: StylePool::new(),
            alt_screen_setup,
            alt_screen_teardown,
            active: false,
            prev: Surface::EMPTY,
            prev_dims: None,
            prev_graphics: Vec::new(),
            painted: None,
            rewritten: Vec::new(),
            frame_hint: 1024,
        }
    }

    /// 颜色深度为 `depth` 的终端的渲染器（tui-kit.md §3.4）：换一个这个深度的样式池并丢掉 diff 的基线。在第一帧之前调用；
    /// 已经涂过的地要到下一次换地才按新深度重设终端的默认色。
    pub fn with_depth(mut self, depth: ColorDepth) -> Renderer {
        self.pool = StylePool::with_depth(depth);
        self.invalidate();
        self
    }

    /// 把终端的默认色设成地 `b` 的字节（OSC 10/11），颜色按池的深度降采样：256 色时写成格子的标准 RGB，终端的边距与
    /// 格子的地一样；16 色时丢掉（没有可写的 RGB）。真彩时与以前相同。
    fn set_ground(&self, b: &Backdrop) -> String {
        let depth = self.pool.depth();
        if depth == ColorDepth::TrueColor {
            return ansi::set_colors(b.fg, b.bg);
        }
        let osc = |c: Color| match depth.reduce(c) {
            Color::Indexed(n) => {
                color::indexed_rgb(n).map_or(Color::Default, |(r, g, b)| Color::rgb(r, g, b))
            }
            other => other,
        };
        ansi::set_colors(osc(b.fg), osc(b.bg))
    }

    /// 画一帧 `rows` × `width` 个格子，返回要写出的字节；与上一帧相同（尺寸、格子、图形都相同）时返回 `None`，一个字节也
    /// 不写。`lines` 裁或补到正好 `rows` 行；`graphics` 画在文字之后；`backdrop` 是主题涂的地（如果有）。
    pub fn render(
        &mut self,
        width: usize,
        rows: usize,
        lines: &[StyledLine],
        graphics: &[Graphic],
        backdrop: Option<&Backdrop>,
    ) -> Option<String> {
        // 地换了（换主题、换侧）：样式重新按新地写，整屏重画。
        let regrounded =
            self.active && backdrop.map(Backdrop::colors) != self.painted.map(|p| p.colors());
        // 颜色没变、只是还法变了（会话中外观变过）：记下，不用重画。
        if self.active && !regrounded {
            self.painted = backdrop.copied();
        }
        self.pool.reground(backdrop.map(Backdrop::colors));
        let next = self.surface(width, rows, lines);
        let same_dims = self.active && self.prev_dims == Some((width, rows)) && !regrounded;

        // no-op 快路径：同尺寸、逐格相同、图形相同。
        if same_dims && same_grid(&self.prev, &next) && graphics == self.prev_graphics.as_slice() {
            return None;
        }

        let mut out = Emit {
            sb: String::with_capacity(self.frame_hint),
            row: 0,
            col: 0,
            pool: &self.pool,
        };
        out.sb.push_str(ansi::SYNC_BEGIN);
        out.sb.push_str(ansi::HIDE_CURSOR);
        self.rewritten = vec![false; rows];

        // 不见了或变了的图形：先去掉。
        let gone: Vec<&Graphic> = self
            .prev_graphics
            .iter()
            .filter(|g| !graphics.contains(g))
            .collect();
        for g in &gone {
            out.sb.push_str(&g.erase);
        }

        let fresh = !same_dims;
        if !self.active {
            // 进备用屏（存下主屏的光标），push 它的键盘增强（kitty 按缓冲记），关自动换行（见 [`ansi::WRAP_OFF`]），
            // 清屏，整屏画。设置序列在这一帧的同步输出里，紧跟 `?1049h`。
            out.sb.push_str(ansi::ALT_SCREEN_ENTER);
            out.sb.push_str(&self.alt_screen_setup);
            out.sb.push_str(ansi::WRAP_OFF);
            // 回到备用屏（首帧、交出终端之后）：再把终端的默认色设成主题的。
            if let Some(b) = backdrop {
                out.sb.push_str(&self.set_ground(b));
            }
            self.painted = backdrop.copied();
            out.sb.push_str(self.pool.base());
            out.sb.push_str(ansi::CLEAR_SCREEN);
            out.sb.push_str(ansi::CURSOR_HOME);
            self.active = true;
            let left = out.paint_fresh(&next);
            out.sb.push_str(self.pool.closing(left));
        } else if fresh {
            // 尺寸变了、invalidate 过或换了地：清屏，整屏重画。
            if regrounded {
                match backdrop {
                    Some(b) => out.sb.push_str(&self.set_ground(b)),
                    // 不再涂地：还给终端它自己的颜色；还成默认时再问一次，回答是终端现在的颜色。
                    None => {
                        if let Some(p) = &self.painted {
                            out.sb.push_str(&p.give_back());
                            if p.restore.is_none() {
                                out.sb.push_str(ansi::ASK_COLORS);
                            }
                        }
                    }
                }
                self.painted = backdrop.copied();
            }
            out.sb.push_str(self.pool.base());
            out.sb.push_str(ansi::CURSOR_HOME);
            out.sb.push_str(ansi::CLEAR_SCREEN);
            let left = out.paint_fresh(&next);
            out.sb.push_str(self.pool.closing(left));
        } else {
            // 稳态：回到绝对原点，再逐格 diff。去掉或移走的图形底下的格子会被重写，这就去掉了 sixel 或 iTerm2 的图片。
            out.sb.push_str(ansi::CURSOR_HOME);
            let dirty;
            let base = if gone.is_empty() {
                &self.prev
            } else {
                dirty = dirtied(&self.prev, &gone);
                &dirty
            };
            let left = out.diff_cells(&next, base, &mut self.rewritten);
            // 文字写完就关上还开着的链接：SGR 重置不关链接，开着的话之后的图片与下一帧写的格子都会落进它。
            if let Some(id) = left {
                out.sb.push_str(self.pool.closing(id));
            }
        }

        // 画新的、移动过的、或格子被重写过的图形。
        for g in graphics {
            let repainted = fresh
                || (g.row..g.row + g.rows).any(|r| self.rewritten.get(r).copied().unwrap_or(false));
            if repainted || !self.prev_graphics.contains(g) {
                out.sb.push_str(ansi::RESET);
                out.absolute(g.row, g.col);
                out.sb.push_str(&g.draw);
            }
        }

        // 修复原因：输入法的候选窗与未上屏的文字画在终端自己的光标处。光标一直隐藏，以前它停在这一帧最后写到的格子：
        // 回答流式输出时是回答的末尾，空闲时是某个淡入或呼吸的格子，于是候选窗在 transcript 里乱跳。现在每帧最后用绝对
        // 定位把隐藏的光标放到输入框的光标格（docs/puffin-tui.md §4.4「终端光标」）。相同的帧走上面的 no-op 快路径：
        // 光标格没变，光标也就不用动。
        if let Some(caret) = next.caret {
            out.absolute(caret / next.width, caret % next.width);
        }

        out.sb.push_str(ansi::RESET);
        out.sb.push_str(ansi::SYNC_END);
        let frame = out.sb;
        self.frame_hint = frame.len().max(1024);

        self.prev = next;
        self.prev_dims = Some((width, rows));
        self.prev_graphics = graphics.to_vec();
        Some(frame)
    }

    /// 丢掉 diff 的基线：下一次 [`Renderer::render`] 清屏并重画每个格子与图形，而不是对着终端可能已经不再显示的屏幕
    /// diff（启动探测改了字形宽度，或 `Cmd::Refresh`）。
    pub fn invalidate(&mut self) {
        self.prev = Surface::EMPTY;
        self.prev_dims = None;
    }

    /// 在备用屏里时离开它（幂等）：在 `?1049l` 之前恢复自动换行、pop 备用屏的键盘增强，一次写。不在备用屏时返回
    /// `None`。
    pub fn exit(&mut self) -> Option<String> {
        if !self.active {
            return None;
        }
        let mut out = String::new();
        // 涂过地的：先把终端的默认色还回去，shell 与编辑器看到的是终端自己的颜色。
        if let Some(p) = &self.painted {
            out.push_str(&p.give_back());
        }
        out.push_str(ansi::WRAP_ON);
        out.push_str(&self.alt_screen_teardown);
        out.push_str(ansi::ALT_SCREEN_EXIT);
        self.painted = None;
        self.active = false;
        self.prev = Surface::EMPTY;
        self.prev_graphics.clear();
        Some(out)
    }

    /// 把 `lines` 铺进正好 `rows` × `width` 的格子网：行太多时裁掉，太少时补空行，diff 总是比较同样大小的格子网。
    fn surface(&mut self, width: usize, rows: usize, lines: &[StyledLine]) -> Surface {
        let clipped = &lines[..lines.len().min(rows)];
        let mut base = Surface::build(width, clipped, &mut self.pool);
        base.keep_margins_clean(&mut self.pool);
        if base.width == width && base.height == rows {
            return base;
        }
        let mut cells = vec![Cell::BLANK; width * rows];
        let n = base.cells.len().min(cells.len());
        cells[..n].copy_from_slice(&base.cells[..n]);
        let caret = base.caret.filter(|&c| c < cells.len());
        Surface::new(width, rows, cells, caret)
    }
}

/// `prev` 把 `gone` 底下的格子标成未知，diff 就会重写它们。
fn dirtied(prev: &Surface, gone: &[&Graphic]) -> Surface {
    let mut cells = prev.cells.clone();
    for g in gone {
        for r in g.row..prev.height.min(g.row + g.rows) {
            for c in g.col..prev.width.min(g.col + g.cols) {
                cells[r * prev.width + c] = Cell::UNKNOWN;
            }
            // 修复原因（Scala 没有这一步）：图形的左边落在宽字形的尾格上时，只有尾格变脏，首格与上一帧相同；diff 从尾格
            // 开始的一段什么都不写、光标不前进，Scala 的 `c = cur.col` 在那里死循环（尾格后面还有脏格时则把它们写偏一列）。
            // 尾格只能连同首格一起重写，所以把首格也标脏。
            if g.col > 0 && g.col < prev.width && prev.at(r, g.col).is_spacer() {
                cells[r * prev.width + g.col - 1] = Cell::UNKNOWN;
            }
        }
    }
    Surface::new(prev.width, prev.height, cells, None)
}

fn same_grid(a: &Surface, b: &Surface) -> bool {
    a.width == b.width && a.height == b.height && a.cells == b.cells
}

/// 这一行有宽度有争议的字形（[`width::risky`]）。
fn row_risky(s: &Surface, row: usize) -> bool {
    (0..s.width).any(|c| {
        let cell = s.at(row, c);
        cell != Cell::UNKNOWN && !cell.is_spacer() && width::risky(cell.code_point())
    })
}

/// 一帧的输出与贯穿它的虚拟光标。
struct Emit<'a> {
    sb: String,
    row: usize,
    col: usize,
    pool: &'a StylePool,
}

impl Emit<'_> {
    fn csi(&mut self, n: usize, fin: char) {
        let _ = write!(self.sb, "{}{n}{fin}", ansi::CSI);
    }

    /// 绝对定位到 0 起的 (`row`, `col`)（`CSI row+1;col+1 H`）；不动虚拟光标（之后只有绝对定位或帧尾）。
    fn absolute(&mut self, row: usize, col: usize) {
        let _ = write!(self.sb, "{}{};{}H", ansi::CSI, row + 1, col + 1);
    }

    fn push_glyph(&mut self, cell: Cell) {
        self.sb.push(cell.code_point());
    }

    /// `next` 对 `prev` 的逐格 diff，发出最少的光标移动与合并样式的连续段。假定光标已在格子网原点、两个格子网同尺寸。
    ///
    /// 含宽度有争议的字形（[`width::risky`]，在任一格子网里——旧行在屏幕上的范围与新行的一样不可知）的行绕过逐格
    /// diff，整行重写来围住它：终端把某个字形画得与 [`width`] 不同时，它右边的相对移动落在错的列，留下模型对模型的 diff
    /// 永远看不到也修不了的残影。返回留在屏幕上的样式 id（什么都没写时 `None`：屏幕上的 SGR 未知）。
    fn diff_cells(
        &mut self,
        next: &Surface,
        prev: &Surface,
        rewritten: &mut [bool],
    ) -> Option<u32> {
        let mut cur_style = None;
        for (row, rewrote) in rewritten.iter_mut().enumerate().take(next.height) {
            let Some(first) = (0..next.width).find(|&c| next.at(row, c) != prev.at(row, c)) else {
                continue;
            };
            if row_risky(next, row) || row_risky(prev, row) {
                cur_style = Some(self.rewrite_row(next, row, cur_style));
                *rewrote = true;
            } else {
                let mut c = first;
                while c < next.width {
                    if next.at(row, c) == prev.at(row, c) {
                        c += 1;
                    } else {
                        self.move_to(row, c);
                        cur_style = self.emit_run(next, prev, row, c, cur_style);
                        // Scala 是 `c = cur.col`：这一段只跳过了尾格（或零宽格）时光标不前进，那里会一直转下去。至少前进一格；
                        // 能走到的情况里 `self.col` 总是大于 `c`，字节不变。
                        c = self.col.max(c + 1);
                    }
                }
            }
        }
        cur_style
    }

    /// 回到样式 0（地）：设基本样式并关上 `from` 的链接；`from` 已是 0 时什么都不写。未知（`None`）也在这里落定。
    fn settle(&mut self, from: Option<u32>) -> u32 {
        if from != Some(0) {
            self.sb.push_str(self.pool.base());
            if let Some(id) = from {
                self.sb.push_str(self.pool.closing(id));
            }
        }
        0
    }

    /// 有争议字形的脏行的围堵：从第 0 列整行重写、擦掉尾巴、以 `\r` 结束让光标列绝对地重新对齐。把某个字形画得与
    /// [`width`] 不同的终端于是把这一行的尾巴画偏——但每帧都一样、只在这一行里、不留旧的格子。返回留下的样式 id。
    fn rewrite_row(&mut self, next: &Surface, row: usize, start: Option<u32>) -> u32 {
        self.move_to(row, 0);
        let mut cur_style = self.settle(start);
        cur_style = self.emit_full_row(next, row, cur_style);
        if self.col < next.width {
            cur_style = self.settle(Some(cur_style));
            self.sb.push_str(ansi::ERASE_TO_EOL); // 按地的样式擦掉尾巴
        } else {
            // 修复原因：整行写满时，自动换行关着的终端把光标停在最后一列，这时擦到行尾会把最后一格擦掉（渲染基准的屏幕
            // 自洽检查发现板的边缘在最右一列缺一格）。写满的行没有尾巴可擦；但终端把某个字形画得比 Width 窄时，它只写到
            // 倒数第二格，最后一格留着上一帧的字。所以用 `\r` 绝对对齐后把最后一个字形再写一次：两种终端的最后一格都对。
            let mut c = next.width - 1;
            if next.at(row, c).is_spacer() {
                c -= 1;
            }
            let cell = next.at(row, c);
            let sid = cell.style_id();
            self.sb.push_str(ansi::CARRIAGE_RETURN);
            self.csi(c, 'C');
            if sid != cur_style {
                self.sb.push_str(self.pool.transition(cur_style, sid));
                cur_style = sid;
            }
            self.push_glyph(cell);
        }
        cur_style = self.settle(Some(cur_style));
        self.sb.push_str(ansi::CARRIAGE_RETURN); // 绝对地重新对齐
        self.col = 0;
        cur_style
    }

    /// 从当前行往下画整个格子网，行之间是 `\r\n`。假定光标在第 0 行第 0 列。返回留下的样式 id。
    fn paint_fresh(&mut self, next: &Surface) -> u32 {
        self.sb.push_str(self.pool.base());
        let mut cur_style = 0;
        for row in 0..next.height {
            if row > 0 {
                self.sb.push_str("\r\n");
                self.row = row;
                self.col = 0;
            }
            cur_style = self.emit_full_row(next, row, cur_style);
        }
        cur_style
    }

    /// 写出一整行（整屏画时用）：到最后一个不是空格子的格子为止，合并样式段，跳过宽字形的尾格。
    fn emit_full_row(&mut self, s: &Surface, row: usize, start: u32) -> u32 {
        let Some(last) = (0..s.width).rev().find(|&c| s.at(row, c) != Cell::BLANK) else {
            return start;
        };
        let mut cur_style = start;
        let mut col = 0;
        while col <= last {
            let cell = s.at(row, col);
            if cell.is_spacer() {
                col += 1;
                continue;
            }
            let sid = cell.style_id();
            if sid != cur_style {
                self.sb.push_str(self.pool.transition(cur_style, sid));
                cur_style = sid;
            }
            self.push_glyph(cell);
            col += self.advance(cell);
        }
        cur_style
    }

    /// 写出从 (`row`, `start_col`) 起连续的变化格子，停在第一个与 `prev` 相同的格子。返回留下的样式 id；`self.col` 前进到
    /// 这一段之后。
    fn emit_run(
        &mut self,
        next: &Surface,
        prev: &Surface,
        row: usize,
        start_col: usize,
        start: Option<u32>,
    ) -> Option<u32> {
        let mut cur_style = start;
        let mut col = start_col;
        while col < next.width && next.at(row, col) != prev.at(row, col) {
            let cell = next.at(row, col);
            if cell.is_spacer() {
                // 正常走不到（脏的尾格的宽字首格总是也脏，会把它一起吃掉）；防御地跳过，不写。
                col += 1;
                continue;
            }
            let sid = cell.style_id();
            if cur_style != Some(sid) {
                let seq = match cur_style {
                    None => self.pool.set_sequence(sid),
                    Some(from) => self.pool.transition(from, sid),
                };
                self.sb.push_str(seq);
                cur_style = Some(sid);
            }
            self.push_glyph(cell);
            col += self.advance(cell);
        }
        cur_style
    }

    /// 写过 `cell` 之后虚拟光标前进几列，返回格子网前进几格。
    fn advance(&mut self, cell: Cell) -> usize {
        match cell.width_class() {
            WidthClass::Wide => {
                self.col += 2;
                2
            }
            WidthClass::ZeroWidth => 1,
            WidthClass::Narrow | WidthClass::Spacer => {
                self.col += 1;
                1
            }
        }
    }

    /// 相对移动到 (`to_row`, `to_col`)，更新虚拟光标。水平移动经 `\r`（再向右），与待换行状态无关。
    fn move_to(&mut self, to_row: usize, to_col: usize) {
        // 这里的位移总是严格非零（行）/ 正（列），所以与 ansi::cursor_up / down / right 相同，只是不经它们的 `n == 0`
        // 判断与中间的 String。
        if to_row < self.row {
            self.csi(self.row - to_row, 'A');
        } else if to_row > self.row {
            self.csi(to_row - self.row, 'B');
        }
        self.row = to_row;
        if to_col == 0 {
            self.sb.push_str(ansi::CARRIAGE_RETURN);
            self.col = 0;
        } else if to_col > self.col {
            self.csi(to_col - self.col, 'C');
            self.col = to_col;
        } else if to_col < self.col {
            self.sb.push_str(ansi::CARRIAGE_RETURN);
            self.csi(to_col, 'C');
            self.col = to_col;
        }
    }
}
