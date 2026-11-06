//! 块解析：把一串行用递归下降解析成块的 AST。
//! 移植自 packages/tui/tui/src/zcode/tui/markdown/BlockParser.scala（feat/tui a2bf878b34）。
//!
//! 语法是 GFM 子集：ATX 与 setext 标题、段落、围栏与缩进代码、引用、无序 / 有序列表（紧凑 / 松散、任务项）、
//! 表格、分隔线。行内内容在建块时由 `inline_parser` 解析，所以最终化的块不会再解析。
//!
//! [`parse_spans`] 报告每个顶层块的行范围；增量的 `Document` 靠它最终化除末块以外的一切、只重解析开放的尾段。
//!
//! 行是原文的切片（`&str`），位置按字节；列宽（缩进、列表内容列）按 Scala 的 UTF-16 单元数算，
//! 制表符展开到 4 的倍数。Java 的 `trim`、正则的 `\s` 与 `isDigit` 见 `inline_parser::java_char`。
use super::ast::{Align, Block, Inline, ListItem};
use super::inline_parser;
use super::inline_parser::java_char::{digit_value, is_digit, trim, trim_end_space};

/// 一个顶层块，以及输入里紧跟它的行号（不含）——下一块从这里开始。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BlockSpan {
    pub(crate) block: Block,
    pub(crate) end_line: usize,
}

/// 一次性解析整段文字。
pub fn parse(text: &str) -> Vec<Block> {
    parse_spans(&split_lines(text))
        .into_iter()
        .map(|sp| sp.block)
        .collect()
}

/// 切行：`\r\n` 与单独的 `\r` 都算换行；末尾的换行不产生多余的空行。行是 `text` 的切片。
pub fn split_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let b = text.as_bytes();
    let mut out = Vec::new();
    let (mut start, mut k) = (0, 0);
    while k < b.len() {
        match b[k] {
            b'\n' => {
                out.push(&text[start..k]);
                k += 1;
                start = k;
            }
            b'\r' => {
                out.push(&text[start..k]);
                k += if b.get(k + 1) == Some(&b'\n') { 2 } else { 1 };
                start = k;
            }
            _ => k += 1,
        }
    }
    out.push(&text[start..]);
    if out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out
}

fn parse_blocks(lines: &[&str]) -> Vec<Block> {
    parse_spans(lines).into_iter().map(|sp| sp.block).collect()
}

pub(crate) fn parse_spans(lines: &[&str]) -> Vec<BlockSpan> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if is_blank(lines[i]) {
            i += 1;
        } else {
            let (block, next) = parse_one(lines, i);
            out.push(BlockSpan {
                block,
                end_line: next,
            });
            i = next;
        }
    }
    out
}

/* ---- 从第 i 行开始的一个块；返回（块，下一行） ---- */

fn parse_one(lines: &[&str], i: usize) -> (Block, usize) {
    let line = lines[i];
    if indent_width(line) >= 4 {
        return parse_indented_code(lines, i);
    }
    if let Some((ch, len, info)) = fence_info(line) {
        return parse_fenced(lines, i, ch, len, info);
    }
    if is_atx_heading(line) {
        (parse_atx(line), i + 1)
    } else if is_thematic_break(line) {
        (Block::ThematicBreak, i + 1)
    } else if starts_blockquote(line) {
        parse_quote(lines, i)
    } else if let Some(m) = list_marker(line) {
        parse_list(lines, i, &m)
    } else if i + 1 < lines.len()
        && is_delimiter_row(lines[i + 1])
        && looks_like_table(line, lines[i + 1])
    {
        parse_table(lines, i)
    } else {
        parse_paragraph(lines, i)
    }
}

/* ---- ATX 标题 ---- */

fn hashes(s: &str) -> usize {
    s.bytes().take_while(|&b| b == b'#').count()
}

fn is_atx_heading(line: &str) -> bool {
    let s = &line[indent_chars(line)..];
    let h = hashes(s);
    (1..=6).contains(&h) && matches!(s.as_bytes().get(h), None | Some(b' ' | b'\t'))
}

fn parse_atx(line: &str) -> Block {
    let s = &line[indent_chars(line)..];
    let h = hashes(s);
    let mut content = trim(&s[h..]);
    // 去掉可选的收尾 # 串（前面要有空白，或者整个都是 #）。
    if !content.is_empty() {
        let stripped = strip_closing_hashes(content);
        if !stripped.is_empty() || is_all_hashes(content) {
            content = stripped;
        }
    }
    // h 在 1..=6（is_atx_heading 已判过）。
    Block::Heading {
        level: h as u8,
        content: inline_parser::parse(content),
    }
}

/// `content.replaceAll("\\s+#+\\s*$", "")`：最左的匹配从收尾 # 串前那段空白的起点开始。
fn strip_closing_hashes(content: &str) -> &str {
    let t = trim_end_space(content);
    let without_hash = t.trim_end_matches('#');
    if without_hash.len() == t.len() {
        return content;
    }
    let before = trim_end_space(without_hash);
    if before.len() == without_hash.len() {
        content
    } else {
        before
    }
}

/// `content.matches("#+\\s*")`。
fn is_all_hashes(content: &str) -> bool {
    let t = trim_end_space(content);
    !t.is_empty() && t.bytes().all(|b| b == b'#')
}

/* ---- 分隔线 ---- */

fn is_thematic_break(line: &str) -> bool {
    let mut it = line[indent_chars(line)..]
        .chars()
        .filter(|&c| c != ' ' && c != '\t');
    let Some(first) = it.next() else { return false };
    if first != '-' && first != '*' && first != '_' {
        return false;
    }
    let mut count = 1;
    for c in it {
        if c != first {
            return false;
        }
        count += 1;
    }
    count >= 3
}

/* ---- setext 下划线 ---- */

fn setext_level(line: &str) -> u8 {
    if indent_width(line) >= 4 {
        return 0;
    }
    let s = trim_end_space(&line[indent_chars(line)..]);
    if !s.is_empty() && s.bytes().all(|b| b == b'=') {
        1
    } else if !s.is_empty() && s.bytes().all(|b| b == b'-') {
        2
    } else {
        0
    }
}

/* ---- 围栏代码 ---- */

fn fence_info(line: &str) -> Option<(u8, usize, &str)> {
    let start = indent_chars(line);
    let ch = *line.as_bytes().get(start)?;
    if ch != b'`' && ch != b'~' {
        return None;
    }
    let len = line.as_bytes()[start..]
        .iter()
        .take_while(|&&b| b == ch)
        .count();
    if len < 3 {
        return None;
    }
    let info = trim(&line[start + len..]);
    // 反引号围栏的信息串里不能有反引号。
    if ch == b'`' && info.contains('`') {
        None
    } else {
        Some((ch, len, info))
    }
}

fn parse_fenced(lines: &[&str], i: usize, ch: u8, len: usize, info: &str) -> (Block, usize) {
    let open_indent = indent_width(lines[i]);
    let mut j = i + 1;
    let mut body: Vec<&str> = Vec::new();
    while j < lines.len() {
        let l = lines[j];
        j += 1;
        if is_closing_fence(l, ch, len) {
            break;
        }
        body.push(strip_up_to(l, open_indent));
    }
    let info_opt = (!info.is_empty()).then(|| info.to_owned());
    let lang = info_opt
        .as_ref()
        .map(|s| s.split([' ', '\t']).next().unwrap_or_default().to_owned())
        .filter(|l| !l.is_empty());
    (
        Block::Code {
            info: info_opt,
            lang,
            code: body.join("\n"),
            fenced: true,
        },
        j,
    )
}

fn is_closing_fence(line: &str, ch: u8, open_len: usize) -> bool {
    if indent_width(line) >= 4 {
        return false;
    }
    let s = trim_end_space(&line[indent_chars(line)..]);
    s.len() >= open_len && s.bytes().all(|b| b == ch)
}

/* ---- 缩进代码 ---- */

fn parse_indented_code(lines: &[&str], i: usize) -> (Block, usize) {
    let mut j = i;
    let mut last_non_blank = i;
    while j < lines.len() && (indent_width(lines[j]) >= 4 || is_blank(lines[j])) {
        if !is_blank(lines[j]) {
            last_non_blank = j;
        }
        j += 1;
    }
    // 末尾的空行不属于这块（留在外面）。
    let end = last_non_blank + 1;
    let body: Vec<&str> = lines[i..end]
        .iter()
        .map(|&l| if is_blank(l) { "" } else { strip_up_to(l, 4) })
        .collect();
    (
        Block::Code {
            info: None,
            lang: None,
            code: body.join("\n"),
            fenced: false,
        },
        end,
    )
}

/* ---- 引用 ---- */

fn starts_blockquote(line: &str) -> bool {
    line.as_bytes().get(indent_chars(line)) == Some(&b'>')
}

fn parse_quote(lines: &[&str], i: usize) -> (Block, usize) {
    let mut inner: Vec<&str> = Vec::new();
    let mut j = i;
    while j < lines.len() && starts_blockquote(lines[j]) {
        let rest = &lines[j][indent_chars(lines[j]) + 1..];
        inner.push(rest.strip_prefix(' ').unwrap_or(rest));
        j += 1;
    }
    (Block::Quote(parse_blocks(&inner)), j)
}

/* ---- 列表 ---- */

struct Marker {
    ordered: bool,
    start: u32,
    bullet: char,
    /// 标记之后的字节位置。
    marker_end: usize,
    /// 内容所在的列（UTF-16 单元，制表符按 4 展开）。
    content_indent: usize,
}

fn list_marker(line: &str) -> Option<Marker> {
    let start = indent_chars(line);
    let c = line[start..].chars().next()?;
    let lead = indent_width(&line[..start]);
    if c == '-' || c == '+' || c == '*' {
        let spaces = after_marker(line, start + 1)?;
        return Some(Marker {
            ordered: false,
            start: 1,
            bullet: c,
            marker_end: start + 1,
            content_indent: lead + 1 + spaces,
        });
    }
    if !is_digit(c) {
        return None;
    }
    // 至多 9 位数字（Java 的 isDigit：各种书写系统的十进制数字都算，按位取值）。
    let mut k = start;
    let mut digits = 0;
    let mut value = 0u32;
    for d in line[start..].chars() {
        if digits >= 9 {
            break;
        }
        let Some(v) = digit_value(d).filter(|_| is_digit(d)) else {
            break;
        };
        value = value * 10 + v;
        digits += 1;
        k += d.len_utf8();
    }
    let delim = *line.as_bytes().get(k)?;
    if delim != b'.' && delim != b')' {
        return None;
    }
    let spaces = after_marker(line, k + 1)?;
    Some(Marker {
        ordered: true,
        start: value,
        bullet: char::from(delim),
        marker_end: k + 1,
        content_indent: lead + digits + 1 + spaces,
    })
}

/// 标记的标点之后：要有空格 / 制表符，或者到了行尾。返回吃掉的空白数
/// （超过 4 个时按 1 个算以外的都并进内容，这里取 min(4)；标记后为空时按一个空格的约定）。
fn after_marker(line: &str, at: usize) -> Option<usize> {
    let b = line.as_bytes();
    if at >= b.len() {
        return Some(1); // 空条目
    }
    if b[at] != b' ' && b[at] != b'\t' {
        return None;
    }
    let spaces = b[at..]
        .iter()
        .take_while(|&&c| c == b' ' || c == b'\t')
        .count();
    if at + spaces >= b.len() {
        Some(1)
    } else {
        Some(spaces.min(4))
    }
}

fn same_list_type(a: &Marker, b: &Marker) -> bool {
    a.ordered == b.ordered && a.bullet == b.bullet
}

fn parse_list(lines: &[&str], i: usize, first: &Marker) -> (Block, usize) {
    let mut items = Vec::new();
    let mut j = i;
    let mut loose = false;
    let mut saw_trailing_blank = false;
    let mut first_item = true;
    while j < lines.len() {
        let Some(m) = list_marker(lines[j]).filter(|m| same_list_type(m, first)) else {
            break;
        };
        if saw_trailing_blank && !first_item {
            loose = true;
        }
        let (item_lines, next, inner_blank) = gather_item(lines, j, &m);
        if inner_blank {
            loose = true;
        }
        items.push(to_list_item(parse_blocks(&item_lines)));
        // 看这个条目与下一个标记之间有没有空行。
        let mut b = next;
        let mut blanks = false;
        while b < lines.len() && is_blank(lines[b]) {
            blanks = true;
            b += 1;
        }
        saw_trailing_blank = blanks;
        j = b;
        first_item = false;
    }
    (
        Block::List {
            ordered: first.ordered,
            start: first.start,
            tight: !loose,
            items,
        },
        j,
    )
}

/// 收一个条目的行，去缩进到它的内容列。返回去缩进的行、下一行的下标，以及内容之间是否出现了空行（松散的信号）。
fn gather_item<'a>(lines: &[&'a str], i: usize, m: &Marker) -> (Vec<&'a str>, usize, bool) {
    // 第一行：去掉标记本身（不只是空白），免得内容又解析成同一个列表、无限递归。
    let mut out = vec![drop_columns(lines[i], m.content_indent)];
    let mut j = i + 1;
    let mut inner_blank = false;
    while j < lines.len() {
        let l = lines[j];
        if is_blank(l) {
            // 一串空行只有在条目之后还继续时才算内部的；否则它把这个条目与下一个隔开，
            // 留给调用方（列表的松散在那里判）。
            let mut k = j;
            while k < lines.len() && is_blank(lines[k]) {
                k += 1;
            }
            if k < lines.len() && indent_width(lines[k]) >= m.content_indent {
                inner_blank = true;
                out.extend(std::iter::repeat_n("", k - j));
                j = k;
            } else {
                break;
            }
        } else if indent_width(l) >= m.content_indent {
            out.push(strip_up_to(l, m.content_indent));
            j += 1;
        } else if !starts_new_block(l) {
            out.push(&l[indent_chars(l)..]); // 段落的惰性续行
            j += 1;
        } else {
            break;
        }
    }
    (out, j, inner_blank)
}

fn starts_new_block(line: &str) -> bool {
    is_atx_heading(line)
        || is_thematic_break(line)
        || fence_info(line).is_some()
        || starts_blockquote(line)
        || list_marker(line).is_some()
        || indent_width(line) >= 4
}

fn to_list_item(mut blocks: Vec<Block>) -> ListItem {
    if let Some(Block::Paragraph(content)) = blocks.first_mut()
        && let Some(checked) = task_marker(content)
    {
        return ListItem {
            blocks,
            task: Some(checked),
        };
    }
    ListItem { blocks, task: None }
}

/// 开头的 `[ ]` / `[x]` / `[X]` 加一个空格把条目变成任务项；就地去掉这个前缀。
fn task_marker(content: &mut Vec<Inline>) -> Option<bool> {
    let Some(Inline::Text(t)) = content.first() else {
        return None;
    };
    let checked = t.starts_with("[x] ") || t.starts_with("[X] ");
    if !checked && !t.starts_with("[ ] ") {
        return None;
    }
    let rest = t[4..].to_owned();
    if rest.is_empty() {
        content.remove(0);
    } else {
        content[0] = Inline::Text(rest);
    }
    Some(checked)
}

/* ---- 表格 ---- */

fn is_delimiter_row(line: &str) -> bool {
    let cells = split_table_row(line);
    !cells.is_empty() && cells.iter().all(|c| is_delimiter_cell(trim(c)))
}

/// `t.nonEmpty && t.matches(":?-+:?")`。
fn is_delimiter_cell(t: &str) -> bool {
    let t = t.strip_prefix(':').unwrap_or(t);
    let t = t.strip_suffix(':').unwrap_or(t);
    !t.is_empty() && t.bytes().all(|b| b == b'-')
}

fn looks_like_table(header: &str, delim: &str) -> bool {
    // 某处要有竖线，否则单列的 "---" 分隔行会盖过分隔线或 setext 下划线。
    !is_blank(header)
        && indent_width(header) < 4
        && (header.contains('|') || delim.contains('|'))
        && split_table_row(header).len() == split_table_row(delim).len()
}

fn parse_table(lines: &[&str], i: usize) -> (Block, usize) {
    let header_cells: Vec<Vec<Inline>> = split_table_row(lines[i])
        .into_iter()
        .map(|c| inline_parser::parse(trim(c)))
        .collect();
    let aligns: Vec<Align> = split_table_row(lines[i + 1])
        .into_iter()
        .map(align_of)
        .collect();
    let mut rows = Vec::new();
    let mut j = i + 2;
    while j < lines.len()
        && !is_blank(lines[j])
        && lines[j].contains('|')
        && indent_width(lines[j]) < 4
    {
        let cells = split_table_row(lines[j])
            .into_iter()
            .map(|c| inline_parser::parse(trim(c)))
            .collect();
        rows.push(rectangular(cells, aligns.len()));
        j += 1;
    }
    let header = rectangular(header_cells, aligns.len());
    (
        Block::Table {
            align: aligns,
            header,
            rows,
        },
        j,
    )
}

/// 从 `from` 起正好 `len` 个反引号的那一串的位置。
fn closing_backtick(s: &[u8], from: usize, len: usize) -> Option<usize> {
    let mut j = from;
    while j < s.len() {
        if s[j] == b'`' {
            let m = s[j..].iter().take_while(|&&b| b == b'`').count();
            if m == len {
                return Some(j);
            }
            j += m;
        } else {
            j += 1;
        }
    }
    None
}

fn align_of(cell: &str) -> Align {
    let t = trim(cell);
    match (t.starts_with(':'), t.ends_with(':')) {
        (true, true) => Align::Center,
        (false, true) => Align::Right,
        (true, false) => Align::Left,
        (false, false) => Align::None,
    }
}

fn rectangular(mut cells: Vec<Vec<Inline>>, cols: usize) -> Vec<Vec<Inline>> {
    cells.resize_with(cols, Vec::new);
    cells
}

/// 按没转义的 `|` 切表格的一行，去掉可选的首尾竖线。转义的竖线与行内代码里的竖线留在格里。
/// 除了作分隔的竖线，每个字符都原样进格，所以每格是原行的一段切片。
fn split_table_row(line: &str) -> Vec<&str> {
    let s = &line[indent_chars(line)..];
    let b = s.as_bytes();
    let mut cells = Vec::new();
    let mut cell_start = 0;
    let mut k = 0;
    while k < b.len() {
        match b[k] {
            b'\\' if k + 1 < b.len() => {
                // 反斜杠连同后一个字符一起进格。
                k += 1 + s[k + 1..].chars().next().map_or(1, char::len_utf8);
            }
            b'`' => {
                // 行内代码原样抄，里面的竖线不切。
                let run = b[k..].iter().take_while(|&&c| c == b'`').count();
                k = closing_backtick(b, k + run, run).map_or(k + run, |end| end + run);
            }
            b'|' => {
                cells.push(&s[cell_start..k]);
                k += 1;
                cell_start = k;
            }
            _ => k += 1,
        }
    }
    cells.push(&s[cell_start..]);
    // 去掉首尾竖线造成的空格。
    if !cells.is_empty() && trim(cells[0]).is_empty() && s.starts_with('|') {
        cells.remove(0);
    }
    if cells.last().is_some_and(|c| trim(c).is_empty()) && trim_end_space(s).ends_with('|') {
        cells.pop();
    }
    cells
}

/* ---- 段落（含 setext） ---- */

fn parse_paragraph(lines: &[&str], i: usize) -> (Block, usize) {
    let mut buf = vec![&lines[i][indent_chars(lines[i])..]];
    let mut j = i + 1;
    while j < lines.len() {
        let l = lines[j];
        let sx = setext_level(l);
        if is_blank(l) {
            return (para(&buf), j);
        }
        if sx > 0 {
            return (
                Block::Heading {
                    level: sx,
                    content: inline_parser::parse(&buf.join("\n")),
                },
                j + 1,
            );
        }
        if interrupts_paragraph(l, j, lines) {
            return (para(&buf), j);
        }
        buf.push(&l[indent_chars(l)..]);
        j += 1;
    }
    (para(&buf), j)
}

fn para(buf: &[&str]) -> Block {
    Block::Paragraph(inline_parser::parse(&buf.join("\n")))
}

/// 能打断一个开着的段落的块起点。
fn interrupts_paragraph(line: &str, idx: usize, lines: &[&str]) -> bool {
    is_atx_heading(line)
        || is_thematic_break(line)
        || fence_info(line).is_some()
        || starts_blockquote(line)
        // 只有无序标记或从 1 开始的有序标记能打断段落，且内容要非空（散文里的 "1." 不算）。
        || list_marker(line).is_some_and(|m| (!m.ordered || m.start == 1) && !trim(&line[m.marker_end..]).is_empty())
        || (idx + 1 < lines.len() && is_delimiter_row(lines[idx + 1]) && looks_like_table(line, lines[idx + 1]))
}

/* ---- 共用的辅助 ---- */

/// 整行只有空格与制表符（空行也算）。
pub fn is_blank(line: &str) -> bool {
    line.bytes().all(|b| b == b' ' || b == b'\t')
}

/// 行首空白的字符数（也是字节数：只数空格与制表符）。
fn indent_chars(line: &str) -> usize {
    line.bytes()
        .take_while(|&b| b == b' ' || b == b'\t')
        .count()
}

fn advance_col(col: usize, c: char) -> usize {
    if c == '\t' {
        col + 4 - col % 4
    } else {
        col + c.len_utf16()
    }
}

/// 行首缩进的列数，制表符展开到 4 的倍数。
fn indent_width(line: &str) -> usize {
    line.chars()
        .take_while(|&c| c == ' ' || c == '\t')
        .fold(0, advance_col)
}

/// 从行首去掉正好 `cols` 列，不论是什么字符（用来去掉条目首行的列表标记）。
/// 列按 UTF-16 单元数算，与 Scala 相同。
fn drop_columns(line: &str, cols: usize) -> &str {
    let mut col = 0;
    for (k, c) in line.char_indices() {
        if col >= cols {
            return &line[k..];
        }
        col = advance_col(col, c);
    }
    ""
}

/// 去掉至多 `cols` 列的行首空白。
fn strip_up_to(line: &str, cols: usize) -> &str {
    let mut col = 0;
    for (k, c) in line.char_indices() {
        if col >= cols || (c != ' ' && c != '\t') {
            return &line[k..];
        }
        col = advance_col(col, c);
    }
    ""
}
