//! 格子网：渲染器一帧的快照。移植自 packages/tui/tui/src/zcode/tui/render/Surface.scala（feat/tui a2bf878b34）。
use super::cell::{Cell, WidthClass};
use super::line::StyledLine;
use super::pool::StylePool;
use super::style::{Decor, Style, attr};
use super::width;

/// `width * height` 个格子，行优先。`caret` 是第一个以 [`attr::CARET`] 样式画出的格子的下标。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Surface {
    pub width: usize,
    pub height: usize,
    pub cells: Vec<Cell>,
    pub caret: Option<usize>,
}

impl Surface {
    pub const EMPTY: Surface = Surface {
        width: 0,
        height: 0,
        cells: Vec::new(),
        caret: None,
    };

    pub fn new(width: usize, height: usize, cells: Vec<Cell>, caret: Option<usize>) -> Surface {
        Surface {
            width,
            height,
            cells,
            caret,
        }
    }

    /// 全空的格子网。
    pub fn blank(width: usize, height: usize) -> Surface {
        Surface {
            width,
            height,
            cells: vec![Cell::BLANK; width * height],
            caret: None,
        }
    }

    /// `(row, col)` 的格子；越界时是 [`Cell::BLANK`]（与更小的上一帧 diff 时读得干净）。
    pub fn at(&self, row: usize, col: usize) -> Cell {
        if row < self.height && col < self.width {
            self.cells[row * self.width + col]
        } else {
            Cell::BLANK
        }
    }

    /// 把样式行铺进 `width` 列的格子网（一行一行）。
    ///
    /// 每个码点一个格子：宽 2 的字形占 [`WidthClass::Wide`] 首格加 [`WidthClass::Spacer`] 尾格；零宽记号丢掉（已记录的
    /// 限制——我们的内容用预组合字符）；超出 `width` 的内容裁掉。样式 id 每段 intern 一次。第一个 [`attr::CARET`] 样式
    /// 的段的首格是 `caret`。
    pub fn build(width: usize, lines: &[StyledLine], pool: &mut StylePool) -> Surface {
        let height = lines.len();
        if width == 0 || height == 0 {
            return Surface::blank(width, height);
        }
        let widths = width::current();
        let mut cells = vec![Cell::BLANK; width * height];
        let mut caret = None;
        for (row, line) in lines.iter().enumerate() {
            let base = row * width;
            let mut col = 0usize;
            for sp in &line.spans {
                if col >= width {
                    break;
                }
                let sid = pool.intern(sp.style, &sp.decor);
                if caret.is_none() && !sp.text.is_empty() && sp.style.has_attr(attr::CARET) {
                    caret = Some(base + col);
                }
                for cp in sp.text.chars() {
                    if col >= width {
                        break;
                    }
                    match widths.display_width(cp) {
                        0 => {} // 零宽记号：丢掉
                        2 => {
                            if col + 2 <= width {
                                cells[base + col] = Cell::pack(cp, sid, WidthClass::Wide);
                                cells[base + col + 1] = Cell::pack(' ', sid, WidthClass::Spacer);
                                col += 2;
                            } else {
                                // 宽字形放不进最后一列：补一格空格，这一行到此为止
                                cells[base + col] = Cell::pack(' ', sid, WidthClass::Narrow);
                                col = width;
                            }
                        }
                        _ => {
                            cells[base + col] = Cell::pack(cp, sid, WidthClass::Narrow);
                            col += 1;
                        }
                    }
                }
            }
        }
        Surface {
            width,
            height,
            cells,
            caret,
        }
    }

    /// 写给终端之前（只有渲染器走这一步，[`Surface::build`] 仍是逻辑上的格子）：贴边的、只显示底色的空白换成同色的 `█`。
    ///
    /// 修复原因：iTerm2、Rex 把贴边格子的显式底色延伸进窗口的边距。板的 `▄` `▀` 边没有底色，只有文字行有，于是边距里是一个
    /// 只有一格高的色块，比板矮半格，左右各一个台阶（docs/puffin-tui.md §4.2「板」）。换成没有底色的 `█` 之后颜色不变，
    /// 边距不再被染色；在这一层做，所有整宽的底色（板、代码面板的右沿）都走这里。文字、光标格（反显）、带下划线、删除线或
    /// 链接的格子看得出区别，不换。
    pub fn keep_margins_clean(&mut self, pool: &mut StylePool) {
        const VISIBLE_ON_BLANK: u32 = attr::REVERSE | attr::UNDERLINE | attr::STRIKE;
        if self.width == 0 {
            return;
        }
        for row in 0..self.height {
            for at in [row * self.width, (row + 1) * self.width - 1] {
                let cell = self.cells[at];
                if cell.code_point() != ' ' || cell.width_class() != WidthClass::Narrow {
                    continue;
                }
                let style = pool.style_of(cell.style_id());
                let plain = pool.decor_of(cell.style_id()).is_plain();
                let bg = style.bg_color();
                if !bg.is_default() && style.attrs() & VISIBLE_ON_BLANK == 0 && plain {
                    let block = pool.intern(Style::fg(bg), &Decor::PLAIN);
                    self.cells[at] = Cell::pack('█', block, WidthClass::Narrow);
                }
            }
        }
    }
}
