//! 统计的页的 Context 标签（docs/specs/rust-cli/tui-panels.md §4.2；设计稿 D「方格图」）：`/stats` 卡上 ⏎ 打开，代替正文
//! （`page_frame.rs`、`stats_page.rs`）。从上到下：面包屑、v1 的方格图（换成卡的同一档墨）与图例、一句话、大纲（`context_tree.rs`）、余量、
//! 建议。数据都是内核的 [`ContextUsage`]（`App.context_count`），不另算；数的写法同卡（kit `fit::count`），句子是英文。
//! 行按列摆（设计稿的网格），不用折行的元素：只有建议的说明按词折行。
use std::collections::BTreeSet;

use rust_cli_kernel_api::context_usage::{CategoryKind, ContextUsage};
use rust_cli_tui_kit::app::element::{Wrap, wrap_styled};
use rust_cli_tui_kit::app::layout::Layout;
use rust_cli_tui_kit::app::widgets::fade;
use rust_cli_tui_kit::app::widgets::fit::{MEASURE_MARGIN, MEASURE_MAX, clip, count, pad};
use rust_cli_tui_kit::app::widgets::lists::chosen_style;
use rust_cli_tui_kit::app::widgets::stats::{self, Usage, Window};
use rust_cli_tui_kit::app::widgets::usage::view::failed;
use rust_cli_tui_kit::render::width::string_width;
use rust_cli_tui_kit::render::{Span, Style, StyledLine, attr};
use rust_cli_tui_kit::theme::theme::Theme;

use super::context_tree::{self, Node, Path, Places, Shown};
use super::quantities::PERCENT;

/// “接近满”的建议从这个百分比起。
const NEARLY_FULL_PERCENT: u64 = 80;
/// 规则文件的建议：合计不少于这么多 token、且占窗口不少于这个百分比时出现；列出最大的几个；估计能省下的百分比。
const MEMORY_MIN_TOKENS: u64 = 5_000;
const MEMORY_MIN_PERCENT: u64 = 5;
const MEMORY_LARGEST_FILES: usize = 3;
const MEMORY_SAVING_PERCENT: u64 = 30;
/// 方格图：每行的格数（窗口不到 [`WIDE_WINDOW`] 时）与行数；窗口 ≥ 1M 时每行 20 格。
const GRID_COLS: usize = 10;
const GRID_ROWS: usize = 10;
const WIDE_WINDOW: u64 = 1_000_000;
/// 方格图从第 3 列起；图例离方格图 5 列，要 26 列宽，不够时放在方格图下面。
const GRID_AT: usize = 3;
const LEGEND_GAP: usize = 5;
const LEGEND_WIDTH: usize = 26;
/// 图例的名字补到 10 列，数与占比各右对齐 5 列。
const LEGEND_NAME: usize = 10;
const LEGEND_NUMBER: usize = 5;
/// 一句话、`Counting…` 从第 14 列起（卡的 Context 行的内容那一列）。
const SENTENCE_AT: usize = 14;
/// 一句话的长短两种写法、大纲的列按这个宽度分。
const WIDE: usize = 90;
const MEDIUM: usize = 76;
/// 大纲：开合的记号（与建议的标题）在第 3 列，顶层的记号第 5 列、名字第 7 列，每深一层缩进 2 列。
const MARK_AT: usize = 3;
const SWATCH_AT: usize = 5;
const NAME_AT: usize = 7;
const INDENT: usize = 2;
/// 名字与下一列之间至少空的列；没有 note 列时名字到数的右端前这么多列为止；note 给数留的列。
const NAME_GAP: usize = 2;
const NUMBER_ROOM: usize = 8;
const NOTE_GAP: usize = 7;
/// 从这一层（第三层）起不写占比。
const NO_SHARE_DEPTH: usize = 2;
/// 建议的说明从第 5 列起。
const DETAIL_AT: usize = 5;
/// 不到窗口的 1/200（0.5 %）写 `<1%`。
const UNDER_HALF_PERCENT: u64 = 200;
/// 三种宽度的列（§4.2：note、数的右端、占比的右端、指向）。
const WIDE_COLUMNS: Columns = Columns {
    note: Some(34),
    tokens: 60,
    share: 66,
    pointer: Some(69),
};
const MEDIUM_COLUMNS: Columns = Columns {
    note: Some(30),
    tokens: 54,
    share: 60,
    pointer: Some(63),
};
const NARROW_COLUMNS: Columns = Columns {
    note: None,
    tokens: 40,
    share: 46,
    pointer: None,
};

/// 页要的：主题、宽、答案（还没有时是卡的 Context 行与失败的原因）、开合与选中、每个数变了多少 tick。
pub struct Inputs<'a> {
    pub theme: &'a Theme,
    pub width: usize,
    /// 面包屑（`stats_page::crumb`：右端是页的两个标签）。
    pub crumb: StyledLine,
    pub usage: Option<&'a ContextUsage>,
    /// 答案到之前：卡的数（会话视图的用量，条不分段）。
    pub fallback: &'a Usage,
    pub failed: Option<&'a str>,
    pub open: &'a BTreeSet<Path>,
    pub chosen: &'a [String],
    /// 那个数变了多少 tick（键：`legend/组` 或路径用 `/` 连起来）。
    pub age: &'a dyn Fn(&str) -> i64,
    /// 规则文件的路径怎样写。
    pub places: Places<'a>,
}

/// 页的行与选中的行在第几行（没有大纲时 None）。
pub struct Page {
    pub lines: Vec<StyledLine>,
    pub chosen: Option<usize>,
}

/// 一行按列摆：`at` 补空格到那一列再接上（已经过了那一列时直接接上），`right` 让字在那一列之前结束。
#[derive(Default)]
pub(super) struct Row {
    spans: Vec<Span>,
    w: usize,
}

impl Row {
    pub(super) fn at(mut self, col: usize, spans: Vec<Span>) -> Self {
        if self.w < col {
            self.spans.push(Span::plain(" ".repeat(col - self.w)));
            self.w = col;
        }
        self.w += spans.iter().map(|s| string_width(&s.text)).sum::<usize>();
        self.spans.extend(spans);
        self
    }

    pub(super) fn right(self, end: usize, spans: Vec<Span>) -> Self {
        let w: usize = spans.iter().map(|s| string_width(&s.text)).sum();
        self.at(end.saturating_sub(w), spans)
    }

    pub(super) fn line(self) -> StyledLine {
        StyledLine::new(self.spans)
    }
}

pub(super) fn sp(text: impl Into<String>, style: Style) -> Span {
    Span::new(text.into(), style)
}

/// 占窗口的比例：四舍五入的百分数，不到 0.5 % 写 `<1%`；没有窗口时 None。
fn share(tokens: u64, window: u64) -> Option<String> {
    if window == 0 {
        return None;
    }
    if tokens * UNDER_HALF_PERCENT < window {
        return Some("<1%".to_owned());
    }
    Some(format!("{}%", (tokens * PERCENT + window / 2) / window))
}

fn tokens_of(usage: &ContextUsage, kind: CategoryKind) -> u64 {
    usage
        .categories
        .iter()
        .filter(|c| c.kind == kind)
        .map(|c| c.tokens)
        .sum()
}

/// 自动压缩的余量（自动压缩开着时有）。
fn buffer(usage: &ContextUsage) -> u64 {
    tokens_of(usage, CategoryKind::Buffer)
}

/// 卡的入参：用掉的与窗口、压缩的位置（自动压缩开着且有缓冲时是窗口减去缓冲）、组成。
pub fn window_of(usage: &ContextUsage) -> Window {
    let buffer = buffer(usage);
    Window {
        used: usage.total_tokens,
        max: usage.raw_max_tokens,
        compact_at: (usage.auto_compact && buffer > 0)
            .then(|| usage.raw_max_tokens.saturating_sub(buffer)),
        breakdown: context_tree::breakdown(usage),
    }
}

/// 方格图与图例（v1 的方格图，c7e930d859 `context.rs`；墨是卡的同一档）。
fn grid(i: &Inputs<'_>, usage: &ContextUsage, nodes: &[Node], cols: usize) -> Vec<StyledLine> {
    let theme = i.theme;
    let window = usage.raw_max_tokens;
    let per_row = if window >= WIDE_WINDOW {
        GRID_COLS * 2
    } else {
        GRID_COLS
    };
    let cells = per_row * GRID_ROWS;
    let of = |n: u64| ((n as f64 / window as f64) * cells as f64).round() as usize;
    let mut squares: Vec<(&str, Style)> = Vec::with_capacity(cells);
    for node in nodes {
        let style = stats::step(theme, node.rank.unwrap_or(0));
        for _ in 0..of(node.tokens).max(1) {
            squares.push(("■", style));
        }
    }
    squares.truncate(cells);
    let buffer = buffer(usage);
    let room = cells.saturating_sub(of(buffer));
    while squares.len() < room {
        squares.push(("□", theme.fg3));
    }
    while squares.len() < cells {
        squares.push(("□", theme.line));
    }
    // 图例：用掉的、压缩的位置、空一行、每组一行、空闲与缓冲。
    let win = window_of(usage);
    let used = if stats::near(&win) {
        Style::fg(theme.code)
    } else {
        Style::DEFAULT
    };
    let mut legend: Vec<Vec<Span>> = vec![vec![
        sp(count(usage.total_tokens as f64), used),
        sp(" of ", theme.fg3),
        sp(count(window as f64), theme.fg2),
        sp("   ", Style::DEFAULT),
        sp(format!("{}%", usage.percentage), theme.fg2),
    ]];
    if let Some(at) = win.compact_at {
        legend.push(vec![
            sp("compacts at ", theme.fg3),
            sp(count(at as f64), theme.fg2),
        ]);
    }
    legend.push(Vec::new());
    let item = |mark: Span, name: &str, n: u64, key: Option<&str>| {
        let number = vec![sp(
            format!("{:>LEGEND_NUMBER$}", count(n as f64)),
            theme.fg2,
        )];
        let number = match key {
            Some(key) => fade::fade_in(theme, number, (i.age)(key), fade::TICKS, None),
            None => number,
        };
        let mut row = vec![mark, sp(format!(" {}", pad(name, LEGEND_NAME)), theme.fg3)];
        row.extend(number);
        row.push(sp(
            format!("{:>LEGEND_NUMBER$}", share(n, window).unwrap_or_default()),
            theme.fg3,
        ));
        row
    };
    for node in nodes {
        let mark = sp("■", stats::step(theme, node.rank.unwrap_or(0)));
        let key = format!("legend/{}", node.key);
        legend.push(item(mark, &node.name, node.tokens, Some(&key)));
    }
    let free = tokens_of(usage, CategoryKind::Free);
    legend.push(item(sp("□", theme.fg3), "Free", free, None));
    if buffer > 0 {
        legend.push(item(sp("□", theme.line), "Buffer", buffer, None));
    }
    let legend_at = GRID_AT + per_row * 2 + LEGEND_GAP;
    let beside = cols >= legend_at + LEGEND_WIDTH;
    let mut out: Vec<StyledLine> = Vec::new();
    for (r, chunk) in squares.chunks(per_row).enumerate() {
        let mut row_spans: Vec<Span> = Vec::new();
        for (text, style) in chunk {
            match row_spans.last_mut() {
                Some(last) if last.style == *style => {
                    last.text.to_mut().push_str(&format!("{text} "))
                }
                _ => row_spans.push(sp(format!("{text} "), *style)),
            }
        }
        let mut row = Row::default().at(GRID_AT, row_spans);
        if beside && let Some(item) = legend.get(r) {
            row = row.at(legend_at, item.clone());
        }
        out.push(row.line());
    }
    if !beside {
        out.push(StyledLine::EMPTY);
        out.extend(
            legend
                .into_iter()
                .map(|item| Row::default().at(GRID_AT, item).line()),
        );
    }
    out
}

/// 一句话：每次请求带的（Messages 以外用掉的）与 `/compact` 能省的（Messages）。
fn sentence(theme: &Theme, nodes: &[Node], cols: usize) -> StyledLine {
    let messages: u64 = nodes
        .iter()
        .filter(|n| n.key == "messages")
        .map(|n| n.tokens)
        .sum();
    let every: u64 = nodes.iter().map(|n| n.tokens).sum::<u64>() - messages;
    let n = |v: u64| sp(count(v as f64), theme.fg2);
    let dot = sp("  ·  ", theme.line);
    let spans = if cols >= MEDIUM {
        let mut s = vec![n(every), sp(" comes with every request", theme.fg3)];
        if messages > 0 {
            s.extend([
                dot,
                sp("/compact", theme.fg2),
                sp(" can free up to ", theme.fg3),
                n(messages),
            ]);
        }
        s
    } else {
        let mut s = vec![n(every), sp(" every request", theme.fg3)];
        if messages > 0 {
            s.extend([dot, n(messages), sp(" can go", theme.fg3)]);
        }
        s
    };
    Row::default().at(SENTENCE_AT, spans).line()
}

/// 大纲的列（第几列起）：note、数的右端、占比的右端、指向的命令；窄时先去掉 note，再去掉指向。
struct Columns {
    note: Option<usize>,
    tokens: usize,
    share: usize,
    pointer: Option<usize>,
}

fn columns(cols: usize) -> Columns {
    if cols >= WIDE {
        WIDE_COLUMNS
    } else if cols >= MEDIUM {
        MEDIUM_COLUMNS
    } else {
        NARROW_COLUMNS
    }
}

/// 大纲（或余量）的一行要画的。
struct TreeRow<'a> {
    depth: usize,
    /// 记号：顶层是组的 `■`，余量是 `╌` / `─`。
    swatch: Option<Span>,
    open: Option<bool>,
    chosen: bool,
    name: &'a str,
    note: &'a str,
    tokens: u64,
    quiet: bool,
    share: Option<String>,
    pointer: Option<&'a str>,
    age: i64,
}

fn tree_row(theme: &Theme, cols: usize, r: TreeRow<'_>) -> StyledLine {
    let l = columns(cols);
    let chosen = chosen_style(theme);
    let mark = |open: bool| {
        sp(
            if open { "▾" } else { "▸" },
            if r.chosen { chosen } else { theme.fg3 },
        )
    };
    let mut row = Row::default();
    let name_at = if r.depth == 0 {
        if let Some(open) = r.open {
            row = row.at(MARK_AT, vec![mark(open)]);
        }
        match r.swatch {
            Some(swatch) => {
                row = row.at(SWATCH_AT, vec![swatch]);
                NAME_AT
            }
            None => SWATCH_AT,
        }
    } else {
        let base = NAME_AT + INDENT * (r.depth - 1);
        if let Some(open) = r.open {
            row = row.at(base, vec![mark(open)]);
        }
        base + INDENT
    };
    let ink = if r.chosen {
        chosen
    } else if r.quiet {
        theme.fg3
    } else if r.depth == 0 {
        Style::DEFAULT
    } else {
        theme.fg2
    };
    let limit = l
        .note
        .unwrap_or(l.tokens.saturating_sub(NUMBER_ROOM))
        .saturating_sub(name_at + NAME_GAP);
    row = row.at(name_at, vec![sp(clip(r.name, limit), ink)]);
    if let Some(note_at) = l.note
        && !r.note.is_empty()
    {
        let room = l.tokens.saturating_sub(note_at + NOTE_GAP);
        row = row.at(note_at, vec![sp(clip(r.note, room), theme.fg3)]);
    }
    let number_ink = if r.quiet {
        theme.fg3
    } else if r.depth == 0 {
        Style::DEFAULT
    } else {
        theme.fg2
    };
    let number = fade::fade_in(
        theme,
        vec![sp(count(r.tokens as f64), number_ink)],
        r.age,
        fade::TICKS,
        None,
    );
    row = row.right(l.tokens, number);
    // 第三层不写占比：一列 `<1%` 什么也没说。
    if r.depth < NO_SHARE_DEPTH
        && let Some(share) = r.share
    {
        row = row.right(l.share, vec![sp(share, theme.fg3)]);
    }
    if let (Some(at), Some(pointer)) = (l.pointer, r.pointer)
        && at + string_width(pointer) < cols.saturating_sub(1)
    {
        row = row.at(at, vec![sp(pointer, theme.fg3)]);
    }
    row.line()
}

/// 一条建议：警告（接近满）或提示（规则文件大），说明与能省下的量。
struct Suggestion {
    warning: bool,
    title: String,
    save: Option<u64>,
    detail: String,
}

/// 能从 `ContextUsage` 算出的建议（“接近满”与“规则文件”）。
fn suggestions(usage: &ContextUsage) -> Vec<Suggestion> {
    let window = usage.raw_max_tokens;
    let mut out = Vec::new();
    if usage.total_tokens <= window && usage.percentage >= NEARLY_FULL_PERCENT {
        out.push(Suggestion {
            warning: true,
            title: format!("Context is {}% full", usage.percentage),
            save: None,
            detail: if usage.auto_compact {
                "Autocompact will trigger soon, which discards older messages. Use /compact now to control what gets kept."
            } else {
                "Autocompact is disabled. Use /compact to free space."
            }
            .to_owned(),
        });
    }
    let memory: u64 = usage.memory_files.iter().map(|file| file.tokens).sum();
    if memory >= MEMORY_MIN_TOKENS && memory * PERCENT >= window * MEMORY_MIN_PERCENT {
        let mut largest: Vec<_> = usage.memory_files.iter().collect();
        largest.sort_by_key(|file| std::cmp::Reverse(file.tokens));
        let names: Vec<String> = largest
            .iter()
            .take(MEMORY_LARGEST_FILES)
            .map(|file| format!("{} ({})", file.path, count(file.tokens as f64)))
            .collect();
        out.push(Suggestion {
            warning: false,
            title: format!(
                "Memory files use {} tokens ({})",
                count(memory as f64),
                share(memory, window).unwrap_or_default()
            ),
            save: Some(memory * MEMORY_SAVING_PERCENT / PERCENT),
            detail: format!(
                "Largest: {}. Review and prune stale entries.",
                names.join(", ")
            ),
        });
    }
    // 先列警告，再按节省量从大到小。
    out.sort_by_key(|s| (!s.warning, std::cmp::Reverse(s.save.unwrap_or(0))));
    out
}

fn suggestion_lines(theme: &Theme, usage: &ContextUsage, cols: usize) -> Vec<StyledLine> {
    let list = suggestions(usage);
    if list.is_empty() {
        return Vec::new();
    }
    let mut out = vec![
        StyledLine::EMPTY,
        Row::default()
            .at(
                MARK_AT,
                vec![sp("Suggestions", theme.fg2.with_attr(attr::BOLD))],
            )
            .line(),
    ];
    for item in list {
        let mark = if item.warning {
            sp("⚠", Style::fg(theme.code))
        } else {
            sp("◎", Style::fg(theme.brand))
        };
        let mut first = vec![mark, Span::plain(format!(" {}", item.title))];
        if let Some(save) = item.save {
            first.push(sp(format!("  ·  saves ~{}", count(save as f64)), theme.fg3));
        }
        out.push(Row::default().at(MARK_AT, first).line());
        let indent = vec![Span::plain(" ".repeat(DETAIL_AT))];
        out.extend(Layout::lay(
            &wrap_styled(
                indent.clone(),
                indent,
                vec![sp(item.detail, theme.fg3)],
                Wrap::Word,
            ),
            cols,
        ));
    }
    out
}

/// 页的行（tui-panels.md §4.2），与选中的行在第几行。
pub fn page(i: &Inputs<'_>) -> Page {
    let theme = i.theme;
    let cols = i.width.clamp(1, MEASURE_MAX + MEASURE_MARGIN);
    let mut lines = vec![StyledLine::EMPTY, i.crumb.clone(), StyledLine::EMPTY];
    let Some(usage) = i.usage else {
        // 答案还没到：卡的 Context 行，下面 `Counting…`（失败时是原因）。
        lines.extend(
            stats::context_rows(theme, i.fallback, cols)
                .into_iter()
                .map(StyledLine::new),
        );
        lines.push(StyledLine::EMPTY);
        lines.push(match i.failed {
            Some(reason) => Layout::lay(&failed(theme, reason), cols)
                .into_iter()
                .next()
                .unwrap_or(StyledLine::EMPTY),
            None => Row::default()
                .at(SENTENCE_AT, vec![sp("Counting…", theme.fg3)])
                .line(),
        });
        return Page {
            lines,
            chosen: None,
        };
    };
    let window = usage.raw_max_tokens;
    // 超限只在知道窗口时说：窗口为 0 时任何用量都“超出”，`over_limit` 会说一个 0-token 的窗口。
    if window > 0
        && let Some(over) = usage.over_limit()
    {
        lines.extend(Layout::lay(&failed(theme, &over), cols));
        lines.push(StyledLine::EMPTY);
    }
    let nodes = context_tree::outline(usage, &i.places);
    if window > 0 {
        lines.extend(grid(i, usage, &nodes, cols));
        lines.push(StyledLine::EMPTY);
        lines.push(sentence(theme, &nodes, cols));
    } else {
        // 模型没有窗口：卡的那一行（`125k tokens   the model does not say its window`）。
        let known = Usage {
            last_call_tokens: Some(usage.total_tokens),
            ..Usage::default()
        };
        lines.extend(
            stats::context_rows(theme, &known, cols)
                .into_iter()
                .map(StyledLine::new),
        );
    }
    lines.push(StyledLine::EMPTY);
    let rows = context_tree::visible(&nodes, i.open);
    let at = context_tree::chosen_at(&rows, i.chosen);
    let chosen = lines.len() + at;
    for (n, (path, shown)) in rows.iter().enumerate() {
        let Shown { depth, node, open } = *shown;
        let swatch = node
            .rank
            .filter(|_| depth == 0)
            .map(|rank| sp("■", stats::step(theme, rank)));
        lines.push(tree_row(
            theme,
            cols,
            TreeRow {
                depth,
                swatch,
                open,
                chosen: n == at,
                name: &node.name,
                note: &node.note,
                tokens: node.tokens,
                quiet: node.quiet,
                share: (!node.quiet).then(|| share(node.tokens, window)).flatten(),
                pointer: node.pointer,
                age: (i.age)(&path.join("/")),
            },
        ));
    }
    if window > 0 {
        lines.push(StyledLine::EMPTY);
        let quiet = |swatch: &'static str, name: &'static str, note: &'static str, tokens: u64| {
            tree_row(
                theme,
                cols,
                TreeRow {
                    depth: 0,
                    swatch: Some(sp(swatch, theme.line)),
                    open: None,
                    chosen: false,
                    name,
                    note,
                    tokens,
                    quiet: true,
                    share: share(tokens, window),
                    pointer: None,
                    age: fade::TICKS,
                },
            )
        };
        let buffer = buffer(usage);
        if buffer > 0 {
            lines.push(quiet("╌", "Autocompact buffer", "for compacting", buffer));
        }
        lines.push(quiet("─", "Free", "", tokens_of(usage, CategoryKind::Free)));
    }
    lines.extend(suggestion_lines(theme, usage, cols));
    Page {
        lines,
        chosen: Some(chosen),
    }
}

/// 页上会淡入的数（按路径；图例按组）：比较两次答案时用（tui-panels.md §4.3「变化」）。
pub fn figures(usage: &ContextUsage) -> Vec<(String, u64)> {
    fn walk(node: &Node, path: &mut Vec<String>, out: &mut Vec<(String, u64)>) {
        path.push(node.key.clone());
        out.push((path.join("/"), node.tokens));
        for kid in &node.kids {
            walk(kid, path, out);
        }
        path.pop();
    }
    // 键不看显示的写法。
    let nodes = context_tree::outline(usage, &Places::default());
    let mut out: Vec<(String, u64)> = nodes
        .iter()
        .map(|n| (format!("legend/{}", n.key), n.tokens))
        .collect();
    let mut path = Vec::new();
    for node in &nodes {
        walk(node, &mut path, &mut out);
    }
    out
}

#[cfg(test)]
#[path = "context_page_tests.rs"]
mod tests;
