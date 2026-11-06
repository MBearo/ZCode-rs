//! 只认渲染器写出的那些序列的最小 ANSI 终端模拟器（光标移动、带滚动的 CR / LF、擦到屏尾 / 行尾、可打印文字；SGR、
//! 同步输出、光标可见性对文字网格是空操作）。渲染器的输出于是可以按看得见的文字与回滚来断言——这是抓光标漂移类 bug
//! 唯一可行的办法。
//!
//! 它另外建模文字网格不管的两份状态，按屏幕缓冲区分的那一类 bug 就能机械地抓到：哪个屏幕缓冲在用（`?1049h` / `?1049l`），
//! 以及每个缓冲各一个 kitty 键盘增强栈（`CSI >…u` 压到在用的缓冲的栈上，`CSI <u` 弹出）。文字网格只有一份（不随缓冲切换），
//! 与这些测试对画出的文字的断言相符；只有 [`TermEmu::enhancement_active`] / [`TermEmu::enhancement_depth`] 看得见每个缓冲的栈。
//!
//! `term_width` 是这个模拟器自己对宽度的意见，缺省是 [`width::display_width`]。给一个有分歧的函数就模拟了一个 Unicode
//! 表与我们不同的终端——共用宽度表的模拟器在结构上看不见这一类 bug。
//! 移植自 packages/tui/tui/test/src/zcode/tui/render/RendererSuite.scala 的 `TermEmu`（feat/tui a2bf878b34）。
use crate::render::width;

pub(super) struct TermEmu {
    cols: usize,
    rows: usize,
    term_width: Box<dyn Fn(char) -> usize>,
    grid: Vec<Vec<String>>,
    // 每格所在的超链接（OSC 8 的目标，"" 是没有），与 grid 同步滚动和擦除。
    links: Vec<Vec<String>>,
    hyperlink: String,
    pub scrollback: Vec<String>,
    pub row: usize,
    pub col: usize,
    // false 是主屏在用，true 是备用屏。kitty 的 push / pop 作用在在用的缓冲的栈上，所以两边各自平衡；主屏的 push 不影响
    // 备用屏，反之亦然。
    on_alt_buffer: bool,
    main_enhancements: Vec<u32>,
    alt_enhancements: Vec<u32>,
    // DECAWM（`?7h` / `?7l`）。渲染器在备用屏里关掉它，宽度有分歧的终端就把溢出的行裁掉而不是折进下一行；测试断言围堵与
    // 恢复两边。
    autowrap: bool,
}

impl TermEmu {
    pub fn new(cols: usize, rows: usize) -> TermEmu {
        TermEmu::with_widths(cols, rows, width::display_width)
    }

    pub fn with_widths(
        cols: usize,
        rows: usize,
        term_width: impl Fn(char) -> usize + 'static,
    ) -> TermEmu {
        TermEmu {
            cols,
            rows,
            term_width: Box::new(term_width),
            grid: vec![blank_row(cols, " "); rows],
            links: vec![blank_row(cols, ""); rows],
            hyperlink: String::new(),
            scrollback: Vec::new(),
            row: 0,
            col: 0,
            on_alt_buffer: false,
            main_enhancements: Vec::new(),
            alt_enhancements: Vec::new(),
            autowrap: true,
        }
    }

    fn active_enhancements(&mut self) -> &mut Vec<u32> {
        if self.on_alt_buffer {
            &mut self.alt_enhancements
        } else {
            &mut self.main_enhancements
        }
    }

    /// 在用的屏幕缓冲有没有压着 kitty 键盘增强（栈不空）。全屏视图显示时它必须为真。
    pub fn enhancement_active(&mut self) -> bool {
        !self.active_enhancements().is_empty()
    }

    /// 在用的缓冲的增强栈的深度：测试借它证明一个循环是平衡的（备用屏里正好一次 push，回来时没有漏下的）。
    pub fn enhancement_depth(&mut self) -> usize {
        self.active_enhancements().len()
    }

    pub fn autowrap_enabled(&self) -> bool {
        self.autowrap
    }

    pub fn viewport(&self) -> Vec<String> {
        (0..self.rows).map(|r| self.line(r)).collect()
    }

    pub fn line(&self, r: usize) -> String {
        self.grid[r].concat().trim_end().to_string()
    }

    /// `(r, c)` 格子的 OSC 8 目标；不在链接里时是 ""。
    pub fn link_at(&self, r: usize, c: usize) -> &str {
        &self.links[r][c]
    }

    /// 有没有开着的 OSC 8 链接：现在写的文字会落进它。
    pub fn link_open(&self) -> bool {
        !self.hyperlink.is_empty()
    }

    pub fn feed(&mut self, s: &str) {
        let chars: Vec<char> = s.chars().collect();
        let n = chars.len();
        let mut i = 0;
        while i < n {
            let ch = chars[i];
            if ch == '\x1b' {
                i += 1;
                if i < n && chars[i] == '[' {
                    i += 1;
                    let mut params = String::new();
                    while i < n && !chars[i].is_alphabetic() {
                        params.push(chars[i]);
                        i += 1;
                    }
                    let fin = if i < n { chars[i] } else { ' ' };
                    i += 1;
                    self.apply_csi(&params, fin);
                } else if i < n && chars[i] == ']' {
                    // OSC：到 BEL 或 ST（ESC \）为止；只认 OSC 8（超链接），其余忽略。
                    i += 1;
                    let mut body = String::new();
                    while i < n
                        && chars[i] != '\u{7}'
                        && !(chars[i] == '\x1b' && i + 1 < n && chars[i + 1] == '\\')
                    {
                        body.push(chars[i]);
                        i += 1;
                    }
                    i += if i < n && chars[i] == '\u{7}' { 1 } else { 2 };
                    if let Some(rest) = body.strip_prefix("8;") {
                        self.hyperlink = rest
                            .split_once(';')
                            .map(|(_, t)| t.to_string())
                            .unwrap_or_default();
                    }
                }
            } else if ch == '\r' {
                self.col = 0;
                i += 1;
            } else if ch == '\n' {
                self.line_feed();
                i += 1;
            } else {
                self.put_char(ch);
                i += 1;
            }
        }
    }

    fn apply_csi(&mut self, params: &str, fin: char) {
        if params.starts_with('?') {
            // DEC 私有模式。只有备用屏的切换改建模的状态——它选出增强栈在用的缓冲。同步（`?2026`）、光标可见性（`?25`）、
            // 鼠标（`?1000` / `?1006`）不影响文字网格。
            if params == "?1049" {
                self.on_alt_buffer = fin == 'h';
            } else if params == "?7" {
                self.autowrap = fin == 'h';
            }
            return;
        }
        match fin {
            'A' => self.row = self.row.saturating_sub(num(params, 1)),
            'B' => self.row = (self.rows - 1).min(self.row + num(params, 1)),
            'C' => self.col = self.cols.min(self.col + num(params, 1)),
            'D' => self.col = self.col.saturating_sub(num(params, 1)),
            'm' => {} // SGR：文字网格不管
            'H' => {
                // 回原点，或渲染器放图片用的绝对 `CSI row;col H`。
                let mut rc = params.split(';');
                let r = num(rc.next().unwrap_or(""), 1);
                let c = num(rc.next().unwrap_or(""), 1);
                self.row = (self.rows - 1).min(r.saturating_sub(1));
                self.col = (self.cols - 1).min(c.saturating_sub(1));
            }
            'J' => match num(params, 0) {
                0 => self.erase_to_eos(),
                2 => self.clear_screen(),
                3 => self.scrollback.clear(),
                _ => {}
            },
            'K' if num(params, 0) == 0 => self.erase_to_eol(),
            'u' => {
                // 在用的缓冲的栈上的 kitty 键盘协议：`CSI >…u` 压入标志，`CSI <u` 弹出。空栈上的弹出被忽略（真终端也是），
                // 所以不平衡的收尾表现为栈卡住。
                if params.starts_with('>') {
                    let flags = num(params, 0) as u32;
                    self.active_enhancements().push(flags);
                } else if params.starts_with('<') {
                    self.active_enhancements().pop();
                }
            }
            _ => {}
        }
    }

    fn line_feed(&mut self) {
        self.row += 1;
        if self.row >= self.rows {
            let top = self.grid.remove(0);
            self.scrollback.push(top.concat().trim_end().to_string());
            self.links.remove(0);
            self.grid.push(blank_row(self.cols, " "));
            self.links.push(blank_row(self.cols, ""));
            self.row = self.rows - 1;
        }
    }

    fn put_char(&mut self, cp: char) {
        let w = (self.term_width)(cp);
        if w == 0 {
            return;
        }
        if self.col + w > self.cols {
            if self.autowrap {
                self.col = 0;
                self.line_feed();
            } else {
                self.col = self.cols.saturating_sub(w); // 自动换行关着：钉在边缘，盖写
            }
        }
        self.grid[self.row][self.col] = cp.to_string();
        self.links[self.row][self.col].clone_from(&self.hyperlink);
        if w == 2 && self.col + 1 < self.cols {
            self.grid[self.row][self.col + 1] = String::new();
        }
        self.col += w;
        // 写满最后一列后光标停在最后一列（自动换行关着的终端就是这样）：之后的擦除从这一格起。
        if self.col >= self.cols {
            self.col = self.cols - 1;
        }
    }

    fn clear_screen(&mut self) {
        self.grid = vec![blank_row(self.cols, " "); self.rows];
        self.links = vec![blank_row(self.cols, ""); self.rows];
    }

    fn erase_to_eol(&mut self) {
        for c in self.col..self.cols {
            self.grid[self.row][c] = " ".to_string();
            self.links[self.row][c] = String::new();
        }
    }

    fn erase_to_eos(&mut self) {
        self.erase_to_eol();
        for r in self.row + 1..self.rows {
            self.grid[r] = blank_row(self.cols, " ");
            self.links[r] = blank_row(self.cols, "");
        }
    }
}

fn blank_row(cols: usize, fill: &str) -> Vec<String> {
    vec![fill.to_string(); cols]
}

/// 参数里的数字（别的字符丢掉）；没有数字时是 `default`。
fn num(p: &str, default: usize) -> usize {
    let digits: String = p.chars().filter(char::is_ascii_digit).collect();
    if digits.is_empty() {
        default
    } else {
        digits.parse().unwrap_or(default)
    }
}
