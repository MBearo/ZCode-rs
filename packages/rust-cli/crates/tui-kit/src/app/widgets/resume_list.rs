//! `/resume` 的会话列表（docs/puffin-tui.md §4.2「会话列表（/resume）」；docs/specs/rust-cli/tui-kit.md §4.4）：选中的周围
//! 一个窗口的会话（年龄右对齐在 6 格里、一个记号、标题，选中的是 brand，搜索词的字母用 fg，现在的会话说 `current`），然后是
//! 选中的那个的细节行（模型与档位、改动、开始的日子；搜索时是命中的那段），细节变了时淡入。没有头。旧界面列所有文件夹时
//! 用 [`grouped_rows`]：每组上面是文件夹的组名（`@` 面板的组名）；会话带着 [`Dash`] 时（旧界面 ← 的看板，tui-panels.md §2）
//! 记号格与 `current` 那一列是状态，细节行多分支，下面两行是首条输入与最后的回复，列表上面是 [`counts_row`]。纯的；入参是
//! 中性的 [`Session`] 与 [`List`]。
//! 移植自 Puffin 的 shell/resume_ui.rs（`mark_span`、`resume_rows`、`detail_parts`）。
use crate::app::element::Element;
use crate::render::width::string_width;
use crate::render::{Color, Span, Style, attr};
use crate::theme::theme::Theme;

use super::at_panel::rows::clip_path;
use super::fade;
use super::fit::{MEASURE_MAX, clip, pad_left};
use super::lists::{chosen_style, matched, picker_window, row, take_right_cells};
use super::when::{self, AGE_WIDTH, TITLE_COL};

/// 搜索的片段在命中处前后各留这么多字符。
pub const SNIPPET_CONTEXT: usize = 40;

/// 正文里第一次命中 `search`（已是小写）前后的一段（按字符，不切开码点）；没命中时为 None。两个界面的会话列表共用
/// （cli 的 Puffin host 与旧界面的 `/resume`，docs/specs/rust-cli/tui-kit.md §4.2「`/resume`」）。
pub fn snippet(text: &str, search: &str) -> Option<String> {
    let lower = text.to_lowercase();
    // 小写可能改变字节长度：按字符位置对回原文。
    let at = lower.find(search)?;
    let start_char = lower[..at].chars().count();
    let chars: Vec<char> = text.chars().collect();
    let from = start_char.saturating_sub(SNIPPET_CONTEXT);
    let to = (start_char + search.chars().count() + SNIPPET_CONTEXT).min(chars.len());
    let body: String = chars[from..to].iter().collect();
    let body = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let head = if from > 0 { "…" } else { "" };
    let tail = if to < chars.len() { "…" } else { "" };
    Some(format!("{head}{body}{tail}"))
}

/// 会话在搜索 `search`（小写、去掉了首尾空白）下的样子：不搜或标题含它时 `Some(None)`；第一条输入或最后的回答含它时是那一段
/// `Some(Some(片段))`；都不含时 None。
pub fn search_hit(title: &str, first: &str, last: &str, search: &str) -> Option<Option<String>> {
    if search.is_empty() || title.to_lowercase().contains(search) {
        return Some(None);
    }
    Some(Some(
        snippet(first, search).or_else(|| snippet(last, search))?,
    ))
}

/// 一行的那一个记号，按桌面端的次序：上一个回合失败了、会话在等你回答、有你没看过的消息。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mark {
    Failed,
    Waiting,
    Unread,
    #[default]
    None,
}

/// 加的行、删的行与文件。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Changes {
    pub added: u64,
    pub removed: u64,
    pub files: u64,
}

/// 列出的一个会话。时间是 epoch 毫秒（`created_at` 为 0 是不知道）；`title` 已是没有标题时的名字；`snippet` 是搜索词在它的
/// 文字（不是标题）里匹配的地方；`current`：它是现在的会话。
#[derive(Clone, Debug, PartialEq)]
pub struct Session<'a> {
    pub title: String,
    pub created_at: f64,
    pub updated_at: f64,
    pub model: &'a str,
    pub effort: &'a str,
    pub mark: Mark,
    pub changes: Option<Changes>,
    pub snippet: Option<&'a str>,
    pub current: bool,
    /// 会话的文件夹（已按 `~` 写短）：列表列的不只是这个文件夹时才给（旧界面 ⇥ 的「所有文件夹」，tui-kit.md §4.3）；
    /// Puffin 只列这个文件夹，总是 None。
    pub folder: Option<&'a str>,
    /// 旧界面 ← 的看板（列所有文件夹时）：状态与细节；Puffin 总是 None。
    pub dash: Option<Dash<'a>>,
}

/// 看板上一个会话的状态（tui-panels.md §2.1：以前项目页的四种）。墨是项目页的红、绿、青、dim，摆法照 Puffin `/machines`
/// 的行（[`super::machine_rows::mark`]：行首的记号是状态的墨，状态的字在行里）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    NeedsYou,
    Working,
    Ready,
    Inactive,
}

impl State {
    /// 数量那一行的次序。
    pub const ALL: [State; 4] = [Self::NeedsYou, Self::Working, Self::Ready, Self::Inactive];

    /// 行里与数量那一行的字（同 ⌃f 筛空时那一句里的词）。
    pub fn word(self) -> &'static str {
        match self {
            Self::NeedsYou => "needs you",
            Self::Working => "working",
            Self::Ready => "ready",
            Self::Inactive => "inactive",
        }
    }

    /// 记号：等你回答 `●` code（err 只给失败，tui-cockpit.md §5）、在跑 `●` ok、打开着空闲 `○` work、没打开 `○` fg3。
    pub fn mark(self, theme: &Theme) -> Span {
        match self {
            Self::NeedsYou => Span::new("●", Style::fg(theme.code)),
            Self::Working => Span::new("●", Style::fg(theme.ok)),
            Self::Ready => Span::new("○", Style::fg(theme.work)),
            Self::Inactive => Span::new("○", theme.fg3),
        }
    }

    /// 行里的字的墨：等你回答 code、在跑 fg2，其余 fg3。
    fn ink(self, theme: &Theme) -> Style {
        match self {
            Self::NeedsYou => Style::fg(theme.code),
            Self::Working => theme.fg2,
            Self::Ready | Self::Inactive => theme.fg3,
        }
    }
}

/// 看板上一个会话的状态与细节：目录现在的分支、首条输入、最后的回复（没有时为 ""）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dash<'a> {
    pub state: State,
    pub branch: Option<&'a str>,
    pub prompt: &'a str,
    pub reply: &'a str,
}

/// 看板的第一行：各状态的数量（tui-panels.md §2.2）。画在列表那一句（`No session matches “…”.`）的位置，写法照 `/machines`
/// 分步的一行（[`super::machine_rows::step_spans`]：记号、一格、字，项之间三格）。数量为 0 的不写；`chosen`（⌃f 选中的状态）
/// 总是写，字是 fg2。什么都不写时为 None。
pub fn counts_row(
    theme: &Theme,
    counts: &[(State, usize)],
    chosen: Option<State>,
) -> Option<Element> {
    let mut spans = Vec::new();
    for &(state, n) in counts {
        let on = chosen == Some(state);
        if n == 0 && !on {
            continue;
        }
        if !spans.is_empty() {
            spans.push(Span::plain("   "));
        }
        spans.push(state.mark(theme));
        spans.push(Span::plain(" "));
        spans.push(Span::new(
            format!("{n} {}", state.word()),
            if on { theme.fg2 } else { theme.fg3 },
        ));
    }
    (!spans.is_empty()).then(|| row(spans))
}

/// 行尾的字：看板的状态（现在的会话接 ` · current`），否则现在的会话的 `current`。
fn tail_spans(theme: &Theme, x: &Session<'_>) -> Vec<Span> {
    let mut out = Vec::new();
    if let Some(d) = &x.dash {
        out.push(Span::new(d.state.word(), d.state.ink(theme)));
        if x.current {
            out.push(Span::new(" · ", theme.line));
        }
    }
    if x.current {
        out.push(Span::new("current", theme.fg3));
    }
    out
}

/// 列表此刻要画的：`earlier` 是有现在的会话以外的会话，`matches` 是显示的会话，`named_age` 是细节行变了多少 tick。
#[derive(Clone, Copy, Debug)]
pub struct List<'a> {
    pub earlier: bool,
    pub matches: &'a [Session<'a>],
    pub choice: usize,
    pub query: &'a str,
    pub named_age: i64,
}

fn mark_span(theme: &Theme, mark: Mark) -> Span {
    match mark {
        Mark::Failed => Span::new("✗", Style::fg(theme.err)),
        Mark::Waiting => Span::new("●", Style::fg(theme.code)),
        Mark::Unread => Span::new("•", Style::new(theme.brand, Color::Default, attr::BOLD)),
        Mark::None => Span::plain(" "),
    }
}

/// 列表的行：选中的周围一个窗口的会话，然后是选中的那个的细节行（§4.2）。`now` 是墙钟。
pub fn rows(
    theme: &Theme,
    list: &List<'_>,
    width: usize,
    now: f64,
    offset: &dyn Fn(f64) -> i32,
) -> Vec<Element> {
    rows_in(theme, list, width, now, offset, false)
}

/// [`rows`]，会话按文件夹分组（旧界面列所有文件夹时，tui-kit.md §4.3）：`matches` 已按文件夹排好（同一个文件夹的相邻），
/// 窗口里每组第一行与窗口的第一行上面是组名，画法同 `@` 面板的组名（第 3 列起、fg3，组之间不空行；
/// `at_panel::rows`），文件夹太长时在 `/` 处缩短。窗口与 `↑ n more` / `↓ n more` 照 [`rows`]，按会话数，组名不算。
pub fn grouped_rows(
    theme: &Theme,
    list: &List<'_>,
    width: usize,
    now: f64,
    offset: &dyn Fn(f64) -> i32,
) -> Vec<Element> {
    rows_in(theme, list, width, now, offset, true)
}

fn rows_in(
    theme: &Theme,
    list: &List<'_>,
    width: usize,
    now: f64,
    offset: &dyn Fn(f64) -> i32,
    grouped: bool,
) -> Vec<Element> {
    if !list.earlier {
        return vec![row(vec![Span::new(
            "No earlier sessions in this folder.",
            theme.fg3,
        )])];
    }
    let ms = list.matches;
    if ms.is_empty() {
        return vec![row(vec![Span::new(
            format!("No session matches “{}”.", list.query),
            theme.fg3,
        )])];
    }
    let content = width.saturating_sub(5).min(MEASURE_MAX - 3);
    let sel = list.choice.min(ms.len() - 1);
    let (from, n) = picker_window(sel, ms.len());
    let tails: Vec<Vec<Span>> = (from..from + n)
        .map(|i| tail_spans(theme, &ms[i]))
        .collect();
    let tail_w = tails
        .iter()
        .map(|t| t.iter().map(|s| string_width(&s.text)).sum::<usize>())
        .max()
        .unwrap_or(0);
    // 标题之后留出的格：Puffin 是 9 格（`current` 之外不另算）；看板的行留出行尾最宽的字与三格。
    let dashed = (from..from + n).any(|i| ms[i].dash.is_some());
    let reserve = if dashed { tail_w + 3 } else { 9 };
    let title_max = content.saturating_sub(TITLE_COL + reserve).max(8);
    let titles: Vec<String> = (from..from + n)
        .map(|i| clip(&ms[i].title, title_max))
        .collect();
    let name_col = titles.iter().map(|t| string_width(t)).max().unwrap_or(0) + 3;
    let gap = || Span::plain(" ".repeat(TITLE_COL));
    let more = |text: String| row(vec![gap(), Span::new(text, theme.fg3)]);
    let mut rows = Vec::new();
    if from > 0 {
        rows.push(more(format!("↑ {from} more")));
    }
    for i in from..from + n {
        let x = &ms[i];
        if grouped && (i == from || ms[i - 1].folder != x.folder) {
            let folder = x.folder.unwrap_or_default();
            rows.push(row(vec![Span::new(clip_path(folder, content), theme.fg3)]));
        }
        let title = &titles[i - from];
        let age = when::age(now, x.updated_at, offset);
        let mut spans = vec![
            Span::new(pad_left(&age, AGE_WIDTH), theme.fg3),
            Span::plain(" "),
            x.dash
                .map_or_else(|| mark_span(theme, x.mark), |d| d.state.mark(theme)),
            Span::plain(" "),
        ];
        if i == sel {
            spans.push(Span::new(title.clone(), chosen_style(theme)));
        } else {
            spans.extend(matched(theme, title, list.query));
        }
        let tail = &tails[i - from];
        if !tail.is_empty() {
            spans.push(Span::plain(
                " ".repeat(name_col.saturating_sub(string_width(title))),
            ));
            spans.extend(tail.iter().cloned());
        }
        rows.push(row(spans));
    }
    if from + n < ms.len() {
        rows.push(more(format!("↓ {} more", ms.len() - from - n)));
    }
    let parts = detail_parts(
        theme,
        &ms[sel],
        list.query,
        content.saturating_sub(TITLE_COL),
        offset,
    );
    let room = content.saturating_sub(TITLE_COL);
    let extra = dash_lines(theme, &ms[sel], list.query, room);
    for parts in std::iter::once(parts).chain(extra) {
        let mut detail = vec![gap()];
        detail.extend(fade::parts(theme, parts, list.named_age, theme.panel));
        rows.push(row(detail));
    }
    rows
}

/// 搜索词在会话的文字里命中的地方：片段与命中的字节位置（只在小写不改长度、落在字符边界上时）。
fn hit_of<'a>(x: &Session<'a>, query: &str) -> Option<(&'a str, usize)> {
    x.snippet.filter(|_| !query.is_empty()).and_then(|sn| {
        let lower = sn.to_lowercase();
        (lower.len() == sn.len())
            .then(|| lower.find(&query.to_lowercase()))
            .flatten()
            .filter(|i| sn.is_char_boundary(*i) && sn.is_char_boundary(i + query.len()))
            .map(|i| (sn, i))
    })
}

/// 看板的细节行下面的两行（tui-panels.md §2.2）：fg3 的 `› ` 与首条输入、最后的回复，照搜索命中那一段的写法（fg3、空白并成
/// 一格、放不下时 `…`）；没有的不写。没有看板、或细节行是搜索命中的那段时没有。
pub fn dash_lines(
    theme: &Theme,
    x: &Session<'_>,
    query: &str,
    room: usize,
) -> Vec<Vec<(String, Style)>> {
    let Some(d) = &x.dash else {
        return Vec::new();
    };
    if hit_of(x, query).is_some() {
        return Vec::new();
    }
    let flat = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = Vec::new();
    let prompt = flat(d.prompt);
    if !prompt.is_empty() {
        out.push(vec![
            ("› ".to_owned(), theme.fg3),
            (clip(&prompt, room.saturating_sub(2)), theme.fg3),
        ]);
    }
    let reply = flat(d.reply);
    if !reply.is_empty() {
        out.push(vec![(clip(&reply, room), theme.fg3)]);
    }
    out
}

/// 细节行在 `room` 格内的部分：搜索词在会话的文字里匹配的地方（命中处留在视野里）；否则它的模型与档位、它的改动与开始
/// 的时候，不知道的不说。
pub fn detail_parts(
    theme: &Theme,
    x: &Session<'_>,
    query: &str,
    room: usize,
    offset: &dyn Fn(f64) -> i32,
) -> Vec<(String, Style)> {
    if let Some((sn, i)) = hit_of(x, query) {
        let found = &sn[i..i + query.len()];
        let post = &sn[i + query.len()..];
        let pre0 = &sn[..i];
        // 前面的字从左边截断，命中处与它后面的一小段总在视野里。
        let keep = room
            .saturating_sub(string_width(found))
            .saturating_sub(string_width(post).min(16));
        let pre = if string_width(pre0) <= keep {
            pre0.to_owned()
        } else {
            format!("…{}", take_right_cells(pre0, keep.saturating_sub(1)))
        };
        return vec![
            (pre, theme.fg3),
            (found.to_owned(), Style::DEFAULT),
            (post.to_owned(), theme.fg3),
        ];
    }
    let sep = ("  ·  ".to_owned(), theme.line);
    let mut groups: Vec<Vec<(String, Style)>> = Vec::new();
    if let Some(folder) = x.folder {
        groups.push(vec![(folder.to_owned(), theme.fg3)]);
    }
    if let Some(branch) = x.dash.and_then(|d| d.branch) {
        groups.push(vec![(branch.to_owned(), theme.fg3)]);
    }
    if !x.model.is_empty() {
        let mut g = vec![(x.model.to_owned(), theme.fg3)];
        if !x.effort.is_empty() {
            g.push((" · ".to_owned(), theme.line));
            g.push((x.effort.to_owned(), theme.fg3));
        }
        groups.push(g);
    }
    if let Some(c) = &x.changes {
        groups.push(vec![
            (format!("+{}", c.added), Style::fg(theme.ok)),
            (" ".to_owned(), Style::DEFAULT),
            (format!("−{}", c.removed), Style::fg(theme.err)),
            (
                format!(
                    " in {} file{}",
                    c.files,
                    if c.files == 1 { "" } else { "s" }
                ),
                theme.fg3,
            ),
        ]);
    }
    if x.created_at > 0.0 {
        groups.push(vec![(
            format!("started {}", when::short_date(x.created_at, offset)),
            theme.fg3,
        )]);
    }
    let mut out = Vec::new();
    for (i, g) in groups.into_iter().enumerate() {
        if i > 0 {
            out.push(sep.clone());
        }
        out.extend(g);
    }
    out
}
