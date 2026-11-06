//! 行内解析：把一个块的行内内容（`&str`）解析成 `Vec<Inline>`。
//! 移植自 packages/tui/tui/src/zcode/tui/markdown/InlineParser.scala（feat/tui a2bf878b34）。
//!
//! 天然局限在一个块内（从不跨块），这是整个渲染能增量的原因：最终化的块只解析一次行内，
//! 只有还开着的末块会重解析。
//!
//! 算法是 CommonMark 的两遍：从左到右扫描，产出节点链表，把强调的定界符压进定界符栈、
//! 把链接 / 图片的方括号压进方括号栈；然后一遍解析，把配上的定界符变成
//! `Strong` / `Emph` / `Strikethrough`，配上的方括号变成 `Link` / `Image`。没配上的定界符与方括号
//! 留作字面——这正是流式想要的：打了一半的 `**bold` 先显示星号，闭合的到了再一下变粗。
//!
//! 链表用节点数组加下标实现（Scala 用可变对象与引用）。文本按码点（`char`）走；
//! 字符类见 `java_char`：BMP 之外的码点按 Scala 的代理项回答，所以结果与按 UTF-16 走的 Scala 相同。
//! 链接目标、标题与 `<…>` 自动链接的判定在子模块 `link`。
pub(crate) mod java_char;
mod link;

use self::java_char::{is_letter_or_digit, is_regex_space as is_ws};
use self::link::{is_email_autolink, is_uri_autolink};
use super::ast::Inline;

/// 解析一个块的行内内容。
pub fn parse(input: &str) -> Vec<Inline> {
    if input.is_empty() {
        Vec::new()
    } else {
        Parser::new(input).run()
    }
}

/// 把行内内容压回纯文字（图片的替代文字用）。
pub fn plain(content: &[Inline]) -> String {
    fn go(ins: &[Inline], out: &mut String) {
        for i in ins {
            match i {
                Inline::Text(v) | Inline::Code(v) => out.push_str(v),
                Inline::Emph(c) | Inline::Strong(c) | Inline::Strikethrough(c) => go(c, out),
                Inline::Link { content, .. } => go(content, out),
                Inline::Image { alt, .. } => out.push_str(alt),
                Inline::Autolink { uri, .. } => out.push_str(uri),
                Inline::HardBreak | Inline::SoftBreak => out.push(' '),
            }
        }
    }
    let mut out = String::new();
    go(content, &mut out);
    out
}

/* ---- 扫描时建的链表节点 ---- */

/// 链表的空指针。
const NIL: usize = usize::MAX;

enum NodeKind {
    /// 累积的字面文字。
    Text(String),
    /// 已定形的行内（行内代码、自动链接、换行，或包好的强调 / 链接）。展开时被取走，所以是 `Option`。
    Done(Option<Inline>),
    /// 一串 `*`、`_` 或 `~`；解析强调时可能被部分消耗。
    Delim {
        ch: char,
        length: usize,
        can_open: bool,
        can_close: bool,
        in_stack: bool,
    },
    /// `[` 或 `![` 的开括号；见到它的 `]` 时定形（或作废）。
    Bracket { image: bool, active: bool },
}

struct Node {
    kind: NodeKind,
    prev: usize,
    next: usize,
    /// 还在链表里。Scala 的 `follows` 沿链表找，被摘下的节点找不到；这里用标记代替那次遍历。
    linked: bool,
}

struct Parser {
    s: Vec<char>,
    n: usize,
    /// 节点数组；下标 0 是哨兵头（空文字），真正的节点在它后面。下标即创建顺序。
    nodes: Vec<Node>,
    tail: usize,
    /// 定界符栈：节点下标，按扫描顺序。
    delims: Vec<usize>,
    /// 方括号栈：节点下标。
    brackets: Vec<usize>,
    i: usize,
    /// 上一个字符是行首或换行：续行开头的空白被吞掉。
    at_line_start: bool,
}

const HEAD: usize = 0;

impl Parser {
    fn new(input: &str) -> Self {
        let s: Vec<char> = input.chars().collect();
        let n = s.len();
        Parser {
            s,
            n,
            nodes: vec![Node {
                kind: NodeKind::Text(String::new()),
                prev: NIL,
                next: NIL,
                linked: true,
            }],
            tail: HEAD,
            delims: Vec::new(),
            brackets: Vec::new(),
            i: 0,
            at_line_start: true,
        }
    }

    fn push_node(&mut self, kind: NodeKind) -> usize {
        self.nodes.push(Node {
            kind,
            prev: NIL,
            next: NIL,
            linked: true,
        });
        self.nodes.len() - 1
    }

    fn append(&mut self, kind: NodeKind) -> usize {
        let node = self.push_node(kind);
        let tail = self.tail;
        self.nodes[tail].next = node;
        self.nodes[node].prev = tail;
        self.tail = node;
        node
    }

    fn append_done(&mut self, inline: Inline) {
        self.append(NodeKind::Done(Some(inline)));
    }

    fn append_text(&mut self, t: &str) {
        if t.is_empty() {
            return;
        }
        let tail = self.tail;
        if tail != HEAD
            && let NodeKind::Text(text) = &mut self.nodes[tail].kind
        {
            text.push_str(t);
            return;
        }
        self.append(NodeKind::Text(t.to_owned()));
    }

    fn append_char(&mut self, c: char) {
        let mut buf = [0u8; 4];
        self.append_text(c.encode_utf8(&mut buf));
    }

    fn run(mut self) -> Vec<Inline> {
        self.scan();
        self.resolve_emphasis(0);
        let first = self.nodes[HEAD].next;
        self.flatten(first, NIL)
    }

    /* ---- 第一遍：扫描 ---- */

    fn scan(&mut self) {
        while self.i < self.n {
            let c = self.s[self.i];
            match c {
                '\\' => self.scan_backslash(),
                '`' => self.scan_code(),
                '<' => self.scan_autolink(),
                '\n' => self.scan_newline(),
                '*' | '_' | '~' => self.scan_delim(c),
                '!' if self.peek(1) == '[' => {
                    self.append_bracket(true);
                    self.i += 2;
                    self.at_line_start = false;
                }
                '[' => {
                    self.append_bracket(false);
                    self.i += 1;
                    self.at_line_start = false;
                }
                ']' => self.scan_close_bracket(),
                'h' => match self.bare_url_end() {
                    Some(end) => self.scan_bare_url(end),
                    None => self.scan_text(c),
                },
                _ => self.scan_text(c),
            }
        }
    }

    fn append_bracket(&mut self, image: bool) {
        let b = self.append(NodeKind::Bracket {
            image,
            active: true,
        });
        self.brackets.push(b);
    }

    /// 往后第 `d` 个字符；越界时是空格。
    fn peek(&self, d: usize) -> char {
        self.s.get(self.i + d).copied().unwrap_or(' ')
    }

    fn starts_with_at(&self, at: usize, pat: &str) -> bool {
        pat.chars()
            .enumerate()
            .all(|(d, p)| self.s.get(at + d) == Some(&p))
    }

    fn index_of(&self, c: char, from: usize) -> Option<usize> {
        (from..self.n).find(|&k| self.s[k] == c)
    }

    fn collect(&self, from: usize, until: usize) -> String {
        self.s[from..until].iter().collect()
    }

    fn scan_text(&mut self, c: char) {
        if self.at_line_start && (c == ' ' || c == '\t') {
            self.i += 1;
            return;
        }
        self.at_line_start = false;
        self.append_char(c);
        self.i += 1;
    }

    fn scan_backslash(&mut self) {
        let nx = self.peek(1);
        if nx == '\n' {
            self.append_done(Inline::HardBreak);
            self.i += 2;
            self.at_line_start = true;
        } else if is_ascii_punct(nx) {
            self.append_char(nx);
            self.i += 2;
            self.at_line_start = false;
        } else {
            self.append_text("\\");
            self.i += 1;
            self.at_line_start = false;
        }
    }

    fn scan_code(&mut self) {
        let (s, n, i) = (&self.s, self.n, self.i);
        let mut k = 0;
        while i + k < n && s[i + k] == '`' {
            k += 1;
        }
        // 找正好 k 个反引号的闭合串。
        let mut j = i + k;
        let mut found = None;
        while j < n && found.is_none() {
            if s[j] == '`' {
                let mut m = 0;
                while j + m < n && s[j + m] == '`' {
                    m += 1;
                }
                if m == k { found = Some(j) } else { j += m }
            } else {
                j += 1;
            }
        }
        match found {
            None => {
                self.append_text(&"`".repeat(k));
                self.i += k;
                self.at_line_start = false;
            }
            Some(found) => {
                let mut content: Vec<char> = s[i + k..found]
                    .iter()
                    .map(|&c| if c == '\n' { ' ' } else { c })
                    .collect();
                // 内容不全是空格时，去掉首尾各一个空格。
                if content.len() >= 2
                    && content[0] == ' '
                    && content[content.len() - 1] == ' '
                    && content.iter().any(|&c| c != ' ')
                {
                    content.pop();
                    content.remove(0);
                }
                self.append_done(Inline::Code(content.into_iter().collect()));
                self.i = found + k;
                self.at_line_start = false;
            }
        }
    }

    fn scan_autolink(&mut self) {
        let close = self.index_of('>', self.i + 1);
        if let Some(close) = close {
            let inner = &self.s[self.i + 1..close];
            let is_email = if is_uri_autolink(inner) {
                Some(false)
            } else if is_email_autolink(inner) {
                Some(true)
            } else {
                None
            };
            if let Some(is_email) = is_email {
                let uri = self.collect(self.i + 1, close);
                self.append_done(Inline::Autolink { uri, is_email });
                self.i = close + 1;
                self.at_line_start = false;
                return;
            }
        }
        self.append_text("<");
        self.i += 1;
        self.at_line_start = false;
    }

    /// 从这里开始的裸 `http://` / `https://` 地址在哪结束（GFM 扩展自动链接），没有时为 `None`：
    /// 它要在词首、协议后还有东西、且不在链接文字里。地址一直到下一个空白或 `<`，
    /// 去掉末尾的标点与不成对的 `)`。
    fn bare_url_end(&self) -> Option<usize> {
        let (s, n, i) = (&self.s, self.n, self.i);
        let scheme = if self.starts_with_at(i, "https://") {
            8
        } else if self.starts_with_at(i, "http://") {
            7
        } else {
            return None;
        };
        let starts = i == 0 || {
            let p = s[i - 1];
            is_ws(p) || p == '(' || p == '*' || p == '_' || p == '~'
        };
        // 在链接文字里（有开着的 [，后面还有 ](）不算：链接里不套链接。
        let in_link_text = self
            .brackets
            .iter()
            .any(|&b| matches!(self.nodes[b].kind, NodeKind::Bracket { active: true, .. }))
            && (i..n.saturating_sub(1)).any(|k| s[k] == ']' && s[k + 1] == '(');
        if !starts || in_link_text {
            return None;
        }
        let mut j = i + scheme;
        while j < n && !is_ws(s[j]) && s[j] != '<' {
            j += 1;
        }
        while j > i + scheme {
            let last = s[j - 1];
            let trimmed = if "?!.,:*_~'\"".contains(last) {
                true
            } else if last == ')' {
                let url = &s[i..j];
                url.iter().filter(|&&c| c == ')').count()
                    > url.iter().filter(|&&c| c == '(').count()
            } else {
                false
            };
            if !trimmed {
                break;
            }
            j -= 1;
        }
        (j > i + scheme && is_letter_or_digit(s[i + scheme])).then_some(j)
    }

    fn scan_bare_url(&mut self, end: usize) {
        let uri = self.collect(self.i, end);
        self.append_done(Inline::Autolink {
            uri,
            is_email: false,
        });
        self.i = end;
        self.at_line_start = false;
    }

    fn scan_newline(&mut self) {
        // 硬换行是行尾两个以上空格（反斜杠加换行在 scan_backslash 里处理）。
        let mut hard = false;
        let tail = self.tail;
        if let NodeKind::Text(text) = &mut self.nodes[tail].kind {
            hard = text.ends_with("  ");
            let kept = text.trim_end_matches([' ', '\t']).len();
            text.truncate(kept);
        }
        self.append_done(if hard {
            Inline::HardBreak
        } else {
            Inline::SoftBreak
        });
        self.i += 1;
        self.at_line_start = true;
    }

    fn scan_delim(&mut self, ch: char) {
        self.at_line_start = false;
        let (s, n, i) = (&self.s, self.n, self.i);
        let mut len = 0;
        while i + len < n && s[i + len] == ch {
            len += 1;
        }
        let before = if i == 0 { ' ' } else { s[i - 1] };
        let after = if i + len >= n { ' ' } else { s[i + len] };
        let before_ws = is_ws(before);
        let after_ws = is_ws(after);
        let before_punct = is_punct(before);
        let after_punct = is_punct(after);
        let left_flank = !after_ws && (!after_punct || before_ws || before_punct);
        let right_flank = !before_ws && (!before_punct || after_ws || after_punct);
        let (can_open, can_close) = if ch == '_' {
            (
                left_flank && (!right_flank || before_punct),
                right_flank && (!left_flank || after_punct),
            )
        } else {
            (left_flank, right_flank)
        };
        let d = self.append(NodeKind::Delim {
            ch,
            length: len,
            can_open,
            can_close,
            in_stack: true,
        });
        self.delims.push(d);
        self.i += len;
    }

    fn scan_close_bracket(&mut self) {
        self.at_line_start = false;
        self.i += 1; // 吃掉 ']'
        let Some(opener) = self.brackets.pop() else {
            self.append_text("]");
            return;
        };
        let NodeKind::Bracket { image, active } = self.nodes[opener].kind else {
            self.append_text("]");
            return;
        };
        if !active {
            self.append_text("]");
            return;
        }
        match self.parse_link_tail(self.i) {
            Some((dest, title, end)) => {
                if image {
                    let first = self.nodes[opener].next;
                    let alt = plain(&self.flatten(first, NIL));
                    self.splice_wrap(opener, Inline::Image { alt, dest, title });
                } else {
                    // 先解析链接文字里的强调，再把它包起来。
                    self.process_emphasis_from(opener);
                    let first = self.nodes[opener].next;
                    let content = self.flatten(first, NIL);
                    self.splice_wrap(
                        opener,
                        Inline::Link {
                            content,
                            dest,
                            title,
                        },
                    );
                    // 链接里不套链接：更早的链接开括号作废。
                    for &b in &self.brackets {
                        if let NodeKind::Bracket {
                            image: false,
                            active,
                        } = &mut self.nodes[b].kind
                        {
                            *active = false;
                        }
                    }
                }
                self.i = end;
            }
            // 不是链接（不支持引用式链接）：方括号留作字面。
            None => self.append_text("]"),
        }
    }

    /* ---- 链表的辅助（`](` 之后的目标与标题、`<…>` 自动链接的判定在 link.rs） ---- */

    /// 把从 `from`（开括号本身，丢掉）到链尾的一段换成 `repl`。被丢掉的定界符离开栈。
    fn splice_wrap(&mut self, from: usize, repl: Inline) {
        let before = self.nodes[from].prev;
        let repl = self.push_node(NodeKind::Done(Some(repl)));
        self.nodes[before].next = repl;
        self.nodes[repl].prev = before;
        self.tail = repl;
        let mut cur = from;
        while cur != NIL {
            let node = &mut self.nodes[cur];
            node.linked = false;
            if let NodeKind::Delim { in_stack, .. } = &mut node.kind {
                *in_stack = false;
            }
            cur = node.next;
        }
    }

    /// 把链 [from, until) 展开成行内节点，相邻文字合并。展开过的已定形节点被取走：
    /// 每次展开之后这一段都会被摘下（包进外层节点或作为结果返回），不会再读。
    fn flatten(&mut self, from: usize, until: usize) -> Vec<Inline> {
        let mut out = Vec::new();
        let mut text = String::new();
        let mut cur = from;
        while cur != NIL && cur != until {
            let node = &mut self.nodes[cur];
            match &mut node.kind {
                NodeKind::Text(t) => text.push_str(t),
                NodeKind::Delim { ch, length, .. } => {
                    text.extend(std::iter::repeat_n(*ch, *length))
                }
                NodeKind::Done(inline) => {
                    if !text.is_empty() {
                        out.push(Inline::Text(std::mem::take(&mut text)));
                    }
                    if let Some(inline) = inline.take() {
                        out.push(inline);
                    }
                }
                NodeKind::Bracket { image, .. } => text.push_str(if *image { "![" } else { "[" }),
            }
            cur = node.next;
        }
        if !text.is_empty() {
            out.push(Inline::Text(text));
        }
        out
    }

    /* ---- 第二遍：解析强调 ---- */

    /// 解析开括号之后的强调（链接文字在行的其余部分之前先收尾）。
    fn process_emphasis_from(&mut self, opener: usize) {
        // Scala 用 follows 沿链表找第一个在开括号之后的定界符；节点下标即创建顺序、
        // 链上的定界符按创建顺序排列，所以等价于「下标更大且还在链上」。
        let bottom = self
            .delims
            .iter()
            .position(|&d| d > opener && self.nodes[d].linked)
            .unwrap_or(self.delims.len());
        self.resolve_emphasis(bottom);
    }

    fn delim(&self, d: usize) -> (char, usize, bool, bool, bool) {
        match self.nodes[d].kind {
            NodeKind::Delim {
                ch,
                length,
                can_open,
                can_close,
                in_stack,
            } => (ch, length, can_open, can_close, in_stack),
            // 定界符栈里只放定界符节点。
            _ => (' ', 0, false, false, false),
        }
    }

    fn set_in_stack(&mut self, d: usize, value: bool) {
        if let NodeKind::Delim { in_stack, .. } = &mut self.nodes[d].kind {
            *in_stack = value;
        }
    }

    fn resolve_emphasis(&mut self, bottom: usize) {
        // 开定界符的下限按 ch ∈ {*,_,~} × len%3 × canOpen 分 18 格，平铺成数组。
        let mut openers_bottom = [bottom; 18];
        fn key_index(ch: char, length: usize, can_open: bool) -> usize {
            let ch_idx = match ch {
                '*' => 0,
                '_' => 1,
                _ => 2,
            };
            ch_idx * 6 + (length % 3) * 2 + usize::from(can_open)
        }
        let mut ci = bottom;
        while ci < self.delims.len() {
            let closer = self.delims[ci];
            let (cch, clen, copen, cclose, cin) = self.delim(closer);
            if !(cin && cclose) {
                ci += 1;
                continue;
            }
            let key = key_index(cch, clen, copen);
            let lo = openers_bottom[key].max(bottom);
            let mut opener_idx = None;
            let mut oi = ci;
            while oi > lo {
                oi -= 1;
                let (och, olen, oopen, oclose, oin) = self.delim(self.delims[oi]);
                if oin && oopen && och == cch && can_match(och, olen, oclose, clen, copen) {
                    opener_idx = Some(oi);
                    break;
                }
            }
            match opener_idx {
                Some(oi) => {
                    let opener = self.delims[oi];
                    let (och, olen, ..) = self.delim(opener);
                    let used = if och == '~' {
                        olen.min(clen).min(2)
                    } else if olen >= 2 && clen >= 2 {
                        2
                    } else {
                        1
                    };
                    self.wrap(opener, closer, used);
                    for k in oi + 1..ci {
                        self.set_in_stack(self.delims[k], false);
                    }
                    if self.delim(opener).1 == 0 {
                        self.set_in_stack(opener, false);
                    }
                    if self.delim(closer).1 == 0 {
                        self.set_in_stack(closer, false);
                        ci += 1;
                    }
                }
                None => {
                    openers_bottom[key] = ci;
                    if !copen {
                        self.set_in_stack(closer, false);
                    }
                    ci += 1;
                }
            }
        }
    }

    /// 把开与闭之间（不含两端）的内容包进带样式的节点，两端各消耗 `used` 个字符。
    fn wrap(&mut self, opener: usize, closer: usize, used: usize) {
        let first = self.nodes[opener].next;
        let content = self.flatten(first, closer);
        let (och, ..) = self.delim(opener);
        let inner = if och == '~' {
            Inline::Strikethrough(content)
        } else if used == 2 {
            Inline::Strong(content)
        } else {
            Inline::Emph(content)
        };
        let mut cur = first;
        while cur != NIL && cur != closer {
            let node = &mut self.nodes[cur];
            node.linked = false;
            if let NodeKind::Delim { in_stack, .. } = &mut node.kind {
                *in_stack = false;
            }
            cur = node.next;
        }
        let wrapper = self.push_node(NodeKind::Done(Some(inner)));
        for d in [opener, closer] {
            if let NodeKind::Delim { length, .. } = &mut self.nodes[d].kind {
                *length -= used;
            }
        }
        self.nodes[wrapper].prev = opener;
        self.nodes[wrapper].next = closer;
        self.nodes[opener].next = wrapper;
        self.nodes[closer].prev = wrapper;
    }
}

fn can_match(och: char, olen: usize, ocan_close: bool, clen: usize, ccan_open: bool) -> bool {
    if och == '~' {
        return true;
    }
    let odd_match = (ocan_close || ccan_open)
        && (olen + clen).is_multiple_of(3)
        && !(olen.is_multiple_of(3) && clen.is_multiple_of(3));
    !odd_match
}

/* ---- 字符类 ---- */

fn is_ascii_punct(c: char) -> bool {
    "!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~".contains(c)
}

fn is_punct(c: char) -> bool {
    is_ascii_punct(c) || (!is_letter_or_digit(c) && !is_ws(c) && c >= '¡')
}
