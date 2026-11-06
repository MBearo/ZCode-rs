//! 一帧的快照（docs/specs/rust-cli/tui-kit.md §3.7）：runner 交给 `Screen::drawn` 的行按渲染器的规则铺成 kit 的格子网
//! （`Surface::build`），这里把每个格子的字形与样式读出来给断言用。索引是（列，行），颜色与属性是
//! kit 的 `Color` / `Style`（断言直接比 `Ink` 的 token）。
use rust_cli_tui_kit::render::{Color, Style, StylePool, Surface};

/// 一个格子：字形（宽字形的尾格是空串）与样式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShotCell {
    pub symbol: String,
    pub style: Style,
    pub fg: Color,
    pub bg: Color,
}

impl ShotCell {
    pub fn symbol(&self) -> &str {
        &self.symbol
    }

    /// 带这个属性位（`attr::BOLD` 等）。
    pub fn has(&self, bit: u32) -> bool {
        self.style.has_attr(bit)
    }
}

/// 屏幕的范围（列 `left..right`，行 `top..bottom`），与以前 `Buffer.area` 的读法相同。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    pub width: u16,
    pub height: u16,
}

impl Area {
    pub fn left(self) -> u16 {
        0
    }

    pub fn top(self) -> u16 {
        0
    }

    pub fn right(self) -> u16 {
        self.width
    }

    pub fn bottom(self) -> u16 {
        self.height
    }
}

/// 一帧。
#[derive(Debug, Clone)]
pub struct Shot {
    pub area: Area,
    cells: Vec<ShotCell>,
    /// 带 `attr::CARET` 的第一个格子的（列，行）：终端光标停在那里（tui-kit.md §3.2）。
    pub caret: Option<(u16, u16)>,
}

impl Shot {
    pub fn of(surface: &Surface, pool: &StylePool) -> Shot {
        let cells = surface
            .cells
            .iter()
            .map(|cell| {
                let style = pool.style_of(cell.style_id());
                let symbol = if cell.is_spacer() {
                    String::new()
                } else {
                    cell.code_point().to_string()
                };
                ShotCell {
                    symbol,
                    style,
                    fg: style.fg_color(),
                    bg: style.bg_color(),
                }
            })
            .collect();
        let caret = surface.caret.map(|at| {
            (
                (at % surface.width.max(1)) as u16,
                (at / surface.width.max(1)) as u16,
            )
        });
        Shot {
            area: Area {
                width: surface.width as u16,
                height: surface.height as u16,
            },
            cells,
            caret,
        }
    }

    /// 第 `y` 行的文字（宽字形的尾格跳过，去掉行尾空白）。
    pub fn row(&self, y: u16) -> String {
        (0..self.area.width)
            .map(|x| self[(x, y)].symbol.as_str())
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    /// 整屏的文字，行之间换行。
    pub fn text(&self) -> String {
        (0..self.area.height)
            .map(|y| self.row(y))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 第一处 `needle` 所在的（列，行）。
    pub fn find(&self, needle: &str) -> Option<(u16, u16)> {
        (0..self.area.height).find_map(|y| {
            let mut row = String::new();
            let mut starts = Vec::new();
            for x in 0..self.area.width {
                starts.push((row.len(), x));
                row.push_str(&self[(x, y)].symbol);
            }
            let at = row.find(needle)?;
            starts
                .iter()
                .find(|(bytes, x)| *bytes >= at && !self[(*x, y)].symbol.is_empty())
                .map(|(_, x)| (*x, y))
        })
    }

    /// （行，列）的样式（tui-kit.md §3.7 的 `style_at`）。
    pub fn style_at(&self, row: u16, col: u16) -> Style {
        self[(col, row)].style
    }
}

impl std::ops::Index<(u16, u16)> for Shot {
    type Output = ShotCell;

    fn index(&self, (x, y): (u16, u16)) -> &ShotCell {
        &self.cells[usize::from(y) * usize::from(self.area.width) + usize::from(x)]
    }
}

/// 画面的文字：宽字符占两格，第二格跳过。
pub fn text_of(surface: &Surface) -> String {
    (0..surface.height)
        .map(|y| {
            (0..surface.width)
                .map(|x| surface.at(y, x))
                .filter(|cell| !cell.is_spacer())
                .map(rust_cli_tui_kit::render::Cell::code_point)
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 不经 runner 的一屏（单独测一个视图）：`lines` 按 `width` 列铺成 `height` 行的格子网，样式池是真彩的。
pub fn shot_of(lines: &[rust_cli_tui_kit::render::StyledLine], width: u16, height: u16) -> Shot {
    let mut rows = lines.to_vec();
    rows.resize(
        usize::from(height),
        rust_cli_tui_kit::render::StyledLine::default(),
    );
    let mut pool = StylePool::new();
    let surface = Surface::build(usize::from(width), &rows, &mut pool);
    Shot::of(&surface, &pool)
}
