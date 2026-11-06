//! 只认渲染器写出的那种方言的终端解码器（docs/puffin-tui.md §7「渲染基准（L0）」的屏幕自洽）：同步输出、相对与绝对的光标移动、
//! `\r` / `\n`、擦到行尾、清屏、5/2 两种颜色形式与冒号形式的下划线色
//! 的 SGR、OSC 8 链接、备用屏的模式开关、kitty 的图像（APC `G`，跳过不画）。自动换行关着，超出右边缘的字形是错误，
//! 它不认识的序列也是：检查必须响亮地失败，从不猜。宽度用 [`crate::render::width`] 自己的表，所以检查的是 diff 的逻辑，
//! 不是某个终端对字形宽度的意见。
//! 移植自 packages/tui/bench/src/zcode/tui/bench/Vt.scala（feat/tui a2bf878b34 加 bench 的改动）。
use std::fmt;

use crate::render::{Cell, Color, Decor, Style, StylePool, Surface, WidthClass, attr, width};

/// 字节流说一个格子是什么：码点、SGR 可见的样式、decor（下划线色、链接）与宽度类。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VtCell {
    pub cp: char,
    pub style: Style,
    pub decor: Decor,
    pub kind: WidthClass,
}

impl VtCell {
    pub const BLANK: VtCell = VtCell {
        cp: ' ',
        style: Style::DEFAULT,
        decor: Decor::PLAIN,
        kind: WidthClass::Narrow,
    };
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VtError(pub String);

impl fmt::Display for VtError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the frame has {}", self.0)
    }
}

impl std::error::Error for VtError {}

const SYNC_BEGIN: &str = "\x1b[?2026h";
const SYNC_END: &str = "\x1b[?2026l";
const VISIBLE_ATTRS: u32 =
    attr::BOLD | attr::DIM | attr::ITALIC | attr::UNDERLINE | attr::REVERSE | attr::STRIKE;
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0100_0000_01b3;

/// 1×1 空屏的哈希：钉死，Scala 的 VtSuite 断言同一个常量。
pub const PINNED_BLANK_HASH: &str = "71a70387241df2ec";

pub struct Vt {
    pub width: usize,
    pub height: usize,
    grid: Vec<VtCell>,
    pub row: usize,
    pub col: usize,
    // 自动换行关着：写满最后一列后光标停在最后一列（终端就是这样），再写一个字就是越界。渲染器的虚拟光标此时记作 width，
    // 但它之后只用 `\r` 重新对齐，所以两边不会分歧。
    at_end: bool,
    fg: Color,
    bg: Color,
    attrs: u32,
    underline: Color,
    link: String,
    pub in_sync: bool,
    pub cursor_hidden: bool,
    pub alt: bool,
    pub wrap_off: bool,
    /// 上一次喂的帧开头打开了同步输出、末尾关上了。
    pub last_synced: bool,
}

impl Vt {
    pub fn new(width: usize, height: usize) -> Vt {
        Vt {
            width,
            height,
            grid: vec![VtCell::BLANK; width * height],
            row: 0,
            col: 0,
            at_end: false,
            fg: Color::Default,
            bg: Color::Default,
            attrs: 0,
            underline: Color::Default,
            link: String::new(),
            in_sync: false,
            cursor_hidden: false,
            alt: false,
            wrap_off: false,
            last_synced: false,
        }
    }

    pub fn cell(&self, r: usize, c: usize) -> &VtCell {
        &self.grid[r * self.width + c]
    }

    /// 每行的文字：宽字形一次（跳过它的尾格），一行读起来是它的词。
    pub fn plain(&self) -> Vec<String> {
        (0..self.height)
            .map(|r| {
                (0..self.width)
                    .map(|c| self.cell(r, c))
                    .filter(|c| c.kind != WidthClass::Spacer)
                    .map(|c| c.cp)
                    .collect()
            })
            .collect()
    }

    /// 整屏的哈希（[`hash_of`]）：屏幕的身份，与画出它的帧无关，也与语言无关。
    pub fn hash(&self) -> String {
        hash_of(&self.grid)
    }

    /// 这个屏幕与 `expected`（在 `pool` 里 intern 的）第一个不同的格子的描述；处处相同时 `None`。
    pub fn mismatch(&self, expected: &Surface, pool: &StylePool) -> Option<String> {
        if expected.width != self.width || expected.height != self.height {
            return Some(format!(
                "size: screen {}x{}, surface {}x{}",
                self.width, self.height, expected.width, expected.height
            ));
        }
        for (i, got) in self.grid.iter().enumerate() {
            let packed = expected.cells[i];
            let sid = packed.style_id();
            let want = VtCell {
                cp: packed.code_point(),
                style: visible(pool.style_of(sid)),
                decor: pool.decor_of(sid).clone(),
                kind: packed.width_class(),
            };
            if *got != want {
                return Some(format!(
                    "cell ({}, {}): screen {}, surface {}",
                    i / self.width,
                    i % self.width,
                    describe(got),
                    describe(&want)
                ));
            }
        }
        None
    }

    /// 光标停在 `expected` 的光标格上（没有光标格时随便在哪）。
    pub fn caret_parked(&self, expected: &Surface) -> bool {
        match expected.caret {
            None => true,
            Some(caret) => self.row == caret / expected.width && self.col == caret % expected.width,
        }
    }

    /// 解释一帧（一次写）。出错时状态停在出错处。
    pub fn feed(&mut self, frame: &str) -> Result<(), VtError> {
        self.last_synced = frame.starts_with(SYNC_BEGIN) && frame.ends_with(SYNC_END);
        let bytes = frame.as_bytes();
        let n = bytes.len();
        let mut i = 0;
        while i < n {
            let ch = bytes[i];
            if ch == 0x1b {
                if i + 1 >= n {
                    return Err(fail("a lone ESC at the end of the frame"));
                }
                match bytes[i + 1] {
                    b'[' => {
                        // CSI：参数字节 0x30–0x3F，中间字节 0x20–0x2F，一个结尾 0x40–0x7E。
                        let mut j = i + 2;
                        while j < n && (0x20..=0x3f).contains(&bytes[j]) {
                            j += 1;
                        }
                        if j >= n {
                            return Err(fail("an unterminated CSI"));
                        }
                        let fin = bytes[j];
                        if !(0x40..=0x7e).contains(&fin) {
                            return Err(fail(&format!("a bad CSI final byte {fin}")));
                        }
                        self.csi(&frame[i + 2..j], fin as char)?;
                        i = j + 1;
                    }
                    b']' => {
                        // OSC：到 BEL 或 ST。
                        let mut j = i + 2;
                        let mut end = None;
                        let mut next = 0;
                        while end.is_none() && j < n {
                            let c = bytes[j];
                            if c == 0x07 {
                                end = Some(j);
                                next = j + 1;
                            } else if c == 0x1b && j + 1 < n && bytes[j + 1] == b'\\' {
                                end = Some(j);
                                next = j + 2;
                            } else {
                                j += 1;
                            }
                        }
                        let Some(end) = end else {
                            return Err(fail("an unterminated OSC"));
                        };
                        self.osc(&frame[i + 2..end])?;
                        i = next;
                    }
                    b'_' => {
                        // APC：到 ST。只认 kitty 的图像（`ESC _ G…`，欢迎图标）：这里不画图像，图像比的是屏幕的
                        // `graphics` 清单（rust-cli 的 tui-kit.md §5.1「图像」）；别的 APC 照样是错。
                        let mut j = i + 2;
                        while j + 1 < n && !(bytes[j] == 0x1b && bytes[j + 1] == b'\\') {
                            j += 1;
                        }
                        if j + 1 >= n {
                            return Err(fail("an unterminated APC"));
                        }
                        if bytes.get(i + 2) != Some(&b'G') {
                            return Err(fail("an APC it does not know (not a kitty image)"));
                        }
                        i = j + 2;
                    }
                    b'\\' => i += 2, // 落单的 ST：渲染器从不单独写它，但无害
                    other => return Err(fail(&format!("an escape it does not know: ESC {other}"))),
                }
            } else if ch == b'\r' {
                self.col = 0;
                self.at_end = false;
                i += 1;
            } else if ch == b'\n' {
                if self.row + 1 >= self.height {
                    return Err(fail(&format!(
                        "a line feed off the bottom at row {}",
                        self.row
                    )));
                }
                self.row += 1;
                self.at_end = false;
                i += 1;
            } else if ch < 0x20 || ch == 0x7f {
                return Err(fail(&format!(
                    "a control byte {ch} at ({}, {})",
                    self.row, self.col
                )));
            } else {
                let cp = frame[i..].chars().next().unwrap_or(' ');
                i += cp.len_utf8();
                self.put(cp)?;
            }
        }
        Ok(())
    }

    fn style(&self) -> Style {
        Style::new(self.fg, self.bg, self.attrs)
    }

    fn put(&mut self, cp: char) -> Result<(), VtError> {
        let w = width::display_width(cp);
        if w == 0 {
            return Err(fail(&format!(
                "a zero-width glyph U+{:x} at ({}, {}): the renderer drops those",
                cp as u32, self.row, self.col
            )));
        }
        if self.at_end || self.col + w > self.width {
            return Err(fail(&format!(
                "a glyph past the right margin at ({}, {})",
                self.row, self.col
            )));
        }
        let idx = self.row * self.width + self.col;
        let d = Decor {
            underline: self.underline,
            link: self.link.clone(),
        };
        let style = self.style();
        if w == 2 {
            self.grid[idx] = VtCell {
                cp,
                style,
                decor: d.clone(),
                kind: WidthClass::Wide,
            };
            self.grid[idx + 1] = VtCell {
                cp: ' ',
                style,
                decor: d,
                kind: WidthClass::Spacer,
            };
        } else {
            self.grid[idx] = VtCell {
                cp,
                style,
                decor: d,
                kind: WidthClass::Narrow,
            };
        }
        self.col += w;
        if self.col >= self.width {
            self.col = self.width - 1;
            self.at_end = true;
        }
        Ok(())
    }

    fn csi(&mut self, params: &str, fin: char) -> Result<(), VtError> {
        let digits = !params.is_empty() && params.bytes().all(|b| b.is_ascii_digit());
        let count = || -> usize {
            if params.is_empty() {
                1
            } else {
                params.parse().unwrap_or(1)
            }
        };
        match (params, fin) {
            ("?2026", 'h') => self.in_sync = true,
            ("?2026", 'l') => self.in_sync = false,
            ("?25", 'l') => self.cursor_hidden = true,
            ("?25", 'h') => self.cursor_hidden = false,
            ("?1049", 'h') => self.alt = true,
            ("?1049", 'l') => self.alt = false,
            ("?7", 'l') => self.wrap_off = true,
            ("?7", 'h') => self.wrap_off = false,
            (">27" | ">1" | "<", 'u') => {} // kitty 键盘协议的 push / pop
            ("22;0" | "23;0", 't') => {}    // 窗口标题的 push / pop
            ("2", 'J') => {
                self.grid.fill(VtCell::BLANK);
                self.at_end = false;
            }
            ("", 'H') => self.mv(0, 0)?,
            (_, 'H') => {
                let parts: Vec<&str> = params.split(';').collect();
                if parts.len() != 2 {
                    return Err(fail(&format!(
                        "a cursor position it does not know: CSI {params}H"
                    )));
                }
                let r: isize = parts[0].parse().map_err(|_| {
                    fail(&format!(
                        "a cursor position it does not know: CSI {params}H"
                    ))
                })?;
                let c: isize = parts[1].parse().map_err(|_| {
                    fail(&format!(
                        "a cursor position it does not know: CSI {params}H"
                    ))
                })?;
                self.mv(r - 1, c - 1)?;
            }
            (_, 'A') if params.is_empty() || digits => {
                self.mv(self.row as isize - count() as isize, self.col as isize)?
            }
            (_, 'B') if params.is_empty() || digits => {
                self.mv(self.row as isize + count() as isize, self.col as isize)?
            }
            (_, 'C') if params.is_empty() || digits => {
                self.mv(self.row as isize, self.col as isize + count() as isize)?
            }
            (_, 'D') if params.is_empty() || digits => {
                self.mv(self.row as isize, self.col as isize - count() as isize)?
            }
            ("" | "0", 'K') => {
                let base = self.row * self.width;
                for c in self.col..self.width {
                    self.grid[base + c] = VtCell::BLANK;
                }
            }
            (_, 'm') => self.sgr(params)?,
            _ => return Err(fail(&format!("a CSI it does not know: CSI {params}{fin}"))),
        }
        Ok(())
    }

    fn mv(&mut self, r: isize, c: isize) -> Result<(), VtError> {
        if r < 0 || r >= self.height as isize || c < 0 || c >= self.width as isize {
            return Err(fail(&format!("a cursor move off the screen to ({r}, {c})")));
        }
        self.row = r as usize;
        self.col = c as usize;
        self.at_end = false;
        Ok(())
    }

    fn sgr(&mut self, params: &str) -> Result<(), VtError> {
        if params.is_empty() {
            self.reset();
            return Ok(());
        }
        let parts: Vec<&str> = params.split(';').collect();
        let short = || fail(&format!("a short SGR: CSI {params}m"));
        let num = |s: &str| -> Result<u32, VtError> {
            s.parse()
                .map_err(|_| fail(&format!("an SGR it does not know: {s}")))
        };
        let mut i = 0;
        while i < parts.len() {
            let p = parts[i];
            if p.contains(':') {
                // 冒号形式只用于下划线色：58:2::r:g:b 或 58:5:n。
                let sub: Vec<&str> = p.split(':').collect();
                if sub[0] != "58" {
                    return Err(fail(&format!("a colon SGR it does not know: {p}")));
                }
                self.underline = if sub[1] == "2" && sub.len() == 6 {
                    Color::TrueColor(byte(sub[3])?, byte(sub[4])?, byte(sub[5])?)
                } else if sub[1] == "5" && sub.len() == 3 {
                    Color::Indexed(byte(sub[2])?)
                } else {
                    return Err(fail(&format!("an underline colour it does not know: {p}")));
                };
            } else {
                match num(p)? {
                    0 => self.reset(),
                    1 => self.attrs |= attr::BOLD,
                    2 => self.attrs |= attr::DIM,
                    3 => self.attrs |= attr::ITALIC,
                    4 => self.attrs |= attr::UNDERLINE,
                    7 => self.attrs |= attr::REVERSE,
                    9 => self.attrs |= attr::STRIKE,
                    n @ 30..=37 => self.fg = Color::Named((n - 30) as u8),
                    n @ 90..=97 => self.fg = Color::Named((n - 90 + 8) as u8),
                    n @ 40..=47 => self.bg = Color::Named((n - 40) as u8),
                    n @ 100..=107 => self.bg = Color::Named((n - 100 + 8) as u8),
                    n @ (38 | 48) => {
                        let mut next = || -> Result<u32, VtError> {
                            i += 1;
                            let s = parts.get(i).ok_or_else(short)?;
                            num(s)
                        };
                        let c = match next()? {
                            5 => Color::Indexed(next()? as u8),
                            2 => {
                                let r = next()? as u8;
                                let g = next()? as u8;
                                let b = next()? as u8;
                                Color::TrueColor(r, g, b)
                            }
                            other => {
                                return Err(fail(&format!(
                                    "a colour form it does not know: {p};{other}"
                                )));
                            }
                        };
                        if n == 38 {
                            self.fg = c;
                        } else {
                            self.bg = c;
                        }
                    }
                    other => return Err(fail(&format!("an SGR it does not know: {other}"))),
                }
            }
            i += 1;
        }
        Ok(())
    }

    /// SGR 0：样式与下划线色清掉；链接留着（SGR 重置不关 OSC 8）。
    fn reset(&mut self) {
        self.fg = Color::Default;
        self.bg = Color::Default;
        self.attrs = 0;
        self.underline = Color::Default;
    }

    fn osc(&mut self, body: &str) -> Result<(), VtError> {
        if let Some(target) = body.strip_prefix("8;;") {
            self.link = target.to_string();
            Ok(())
        } else if body.starts_with("0;") || body.starts_with("52;") {
            Ok(()) // 标题、剪贴板
        } else {
            Err(fail(&format!("an OSC it does not know: {body}")))
        }
    }
}

fn fail(what: &str) -> VtError {
    VtError(what.to_string())
}

fn byte(s: &str) -> Result<u8, VtError> {
    s.parse()
        .map_err(|_| fail(&format!("a colour component it does not know: {s}")))
}

/// `s` 去掉写不出任何东西的位（[`attr::CARET`]、[`attr::WHOLE`]、[`attr::WRITTEN`]）。
pub fn visible(s: Style) -> Style {
    Style::new(s.fg_color(), s.bg_color(), s.attrs() & VISIBLE_ATTRS)
}

/// 颜色的规范文本：`d` 默认，`n<i>` 调色板，`i<n>` 256 色，`t<r>,<g>,<b>` 真彩。
pub fn color_text(c: Color) -> String {
    match c {
        Color::Default => "d".to_string(),
        Color::Named(i) => format!("n{i}"),
        Color::Indexed(n) => format!("i{n}"),
        Color::TrueColor(r, g, b) => format!("t{r},{g},{b}"),
    }
}

/// 一个格子的一行 ASCII（字形写成十进制码点），[`hash_of`] 跑在它上面：`cp;attrs;fg;bg;ul;link;kind\n`，`attrs` 是可见的
/// SGR 位。
pub fn canonical(c: &VtCell) -> String {
    format!(
        "{};{};{};{};{};{};{}\n",
        c.cp as u32,
        c.style.attrs() & VISIBLE_ATTRS,
        color_text(c.style.fg_color()),
        color_text(c.style.bg_color()),
        color_text(c.decor.underline),
        c.decor.link,
        c.kind as u8
    )
}

/// FNV-1a 64 跑在 `cells`（行优先）的规范文本上，16 位小写十六进制。
pub fn hash_of(cells: &[VtCell]) -> String {
    let mut h = FNV_OFFSET;
    for c in cells {
        for b in canonical(c).bytes() {
            h = (h ^ u64::from(b)).wrapping_mul(FNV_PRIME);
        }
    }
    format!("{h:016x}")
}

fn describe(c: &VtCell) -> String {
    format!(
        "'{}' U+{:x} {:?}/{:?} attrs={} {:?} kind={:?}",
        c.cp,
        c.cp as u32,
        c.style.fg_color(),
        c.style.bg_color(),
        c.style.attrs(),
        c.decor,
        c.kind
    )
}

/// 渲染器为这些行建的格子网：`lines` 裁到 `rows` 行，不够的补空行（Scala 的 `Harness.expected`）。
pub fn expected(width: usize, rows: usize, lines: &[StyledLine], pool: &mut StylePool) -> Surface {
    let clipped = if lines.len() > rows {
        &lines[..rows]
    } else {
        lines
    };
    let mut base = Surface::build(width, clipped, pool);
    base.keep_margins_clean(pool);
    if base.width == width && base.height == rows {
        return base;
    }
    let mut cells = vec![Cell::BLANK; width * rows];
    let n = base.cells.len().min(cells.len());
    cells[..n].copy_from_slice(&base.cells[..n]);
    let caret = base.caret.filter(|&c| c < cells.len());
    Surface::new(width, rows, cells, caret)
}

use crate::render::StyledLine;
