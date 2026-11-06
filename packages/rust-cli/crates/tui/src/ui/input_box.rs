// 改写自 apps/zcode-host/crates/tui/src/ui/input_box.rs（feat/rs 79b6691c55）：状态来自 `view::Transcript`。
//! 输入框（docs/specs/rust-tui.md §8.4），照 Puffin 画（docs/specs/rust-cli/tui-kit.md §4.2「输入框」；docs/puffin-tui.md
//! §4.2「板（slab）」、§4.3「草稿」「折行」「终端光标」，`shell/composer.rs` 的 `composer` / `draft_rows`）：
//!
//! - 板：rich 时整宽的 `panel` 底色块，上下边是 `▄` / `▀`；plain 时上下两条 `line` 色的 `─`（查看子代理时是 `agent` 色、
//!   搜索模式是 `brand`，tui-kit.md §4.3）。板上依次是排队的消息、面板（`?`、斜杠命令、`@` 文件）与输入行（`pane.rs` 组装）。
//! - 输入行：第 1 列是 brand 粗体的 `›`（bash 模式是 `code` 粗体的 `!`，搜索模式是 brand 粗体的 `⌕`），文字从第 3 列起，
//!   续行缩进 3 列；光标是反显的一格，带 `attr::CARET`（终端隐藏的光标停在这里，输入法候选窗跟着它）。空草稿时光标在第 3 列，
//!   占位文字从第 4 列起（fg3）。槽（按键提示）在最后一行的右端（kit `key_hints::on_row`）。
//! - 开头的命令名画成 brand；正好是 `/名字 ` 且光标在末尾时，后面是 fg3 斜体的参数提示，第一个字在光标格里。
//! - 板上面那一行（Puffin 写会话名字的那一行）右对齐写会话标题；浏览输入历史时写 `History k/n`（tui-kit.md §4.3）。
//!
//! 旧界面的逻辑不变：折行与光标位置仍是 `TextArea::rows` / `cursor_position`（宽度 − 3），行数上限 `max_rows`，占位文字
//! 与提示的中英文照旧（tui-kit.md §4.1 第 4 条）。
use rust_cli_tui_kit::app::element::{Element, hr, single_line, slab, stack};
use rust_cli_tui_kit::app::widgets::key_hints::{edge, on_row};
use rust_cli_tui_kit::render::width::string_width;
use rust_cli_tui_kit::render::{Color, Span, Style, StyledLine, attr};
use rust_cli_tui_kit::theme::theme::Theme;
use unicode_segmentation::UnicodeSegmentation;

use super::app::App;
use super::composer::InputMode;
use super::i18n::Lang;
use super::lines::truncate;
use super::task_line::Title;

/// 输入的文字从第 3 列起（Puffin 的 `COMPOSER_INDENT`）：`draw.rs` 按它换算输入框的点击位置。
pub const TEXT_COL: usize = 3;
/// 续行的缩进。
const INDENT: &str = "   ";

/// 占位文字里子代理类型的最大列数（tui-panels.md §1.6）。
const VIEWING_NAME_MAX: usize = 20;
/// 文字区最多的行数：max(3, 屏幕高度 / 2 − 5)。
const MIN_ROWS: usize = 3;
const ROWS_RESERVED: usize = 5;

/// 输入框为空时的占位文字（Puffin `composer`）：`Send a message`；`!` 时 `Run a shell command`；查看子代理时是它的说明（旧界面
/// 才有）。以前新会话写 `Try "…"`（四个例子轮换）、有排队时写 `Queued messages will be sent automatically`、建会话时写
/// `Starting the session…`，其余 `Ask ZCode to do anything`：排队与建会话由栈与尾巴的 `Queued.` 说（tui-kit.md §4.0）。
pub fn placeholder(app: &App) -> String {
    if let Some(viewing) = &app.viewing {
        let kind = truncate(viewing.type_name(), VIEWING_NAME_MAX);
        return super::i18n_panels::viewing_placeholder(app.lang, &kind);
    }
    if app.cockpit_open() {
        return super::cockpit::say(app.lang, "Type to filter", "输入以筛选").to_owned();
    }
    if app.composer.mode == InputMode::Bash {
        return super::i18n_input::bash_placeholder(app.lang).to_owned();
    }
    // 挑选回退的点：草稿为空时什么都不会发出（Puffin `composer`）。
    if app.picking.is_some() {
        return super::rewind_pick::placeholder(app.lang).to_owned();
    }
    app.strings.placeholder.to_owned()
}

/// 文字区的宽度：屏幕宽度 − 3（前面 3 列是记号与空格；Puffin 的 `composer_width`）。绘制、点击定位与上下移动都用它折行。
pub fn text_width(width: usize) -> usize {
    width.saturating_sub(TEXT_COL)
}

/// 文字区最多的行数：max(3, 屏幕高度 / 2 − 5)。
pub fn max_rows(height: usize) -> usize {
    (height / 2).saturating_sub(ROWS_RESERVED).max(MIN_ROWS)
}

// ---- 与 Puffin 同画面的纯函数（同画面测试直接调它们） ----

/// 输入行第 1 列的记号与它两边的空格：`[" ", 记号, " "]`（Puffin `chevron_spans`）。
pub fn lead(mark: &str, style: Style) -> Vec<Span> {
    vec![
        Span::new(" ", Style::DEFAULT),
        Span::new(mark.to_owned(), style),
        Span::new(" ", Style::DEFAULT),
    ]
}

/// 消息的记号：brand 粗体的 `›`。
pub fn chevron(theme: &Theme) -> Vec<Span> {
    lead("›", Style::new(theme.brand, Color::Default, attr::BOLD))
}

/// 空草稿的输入行：记号、光标格（一个空格）、fg3 的占位文字，槽在右端（Puffin `composer` 的空草稿）。
pub fn placeholder_row(
    theme: &Theme,
    lead: Vec<Span>,
    placeholder: &str,
    slot: Vec<Span>,
) -> Element {
    placeholder_row_with(theme, lead, placeholder, slot, true)
}

/// 同 [`placeholder_row`]；`caret` 为假时光标格是空格、不带 `attr::CARET`（请求或页面拿着键，tui-kit.md §4.0「板」）。
fn placeholder_row_with(
    theme: &Theme,
    lead: Vec<Span>,
    placeholder: &str,
    slot: Vec<Span>,
    caret: bool,
) -> Element {
    let mut spans = lead;
    spans.push(Span::new(
        " ",
        if caret { Style::CARET } else { Style::DEFAULT },
    ));
    // 拿键的层或请求开着（没有光标）时打字不进草稿：不写占位字，槽（层的键）放得下（旧界面的占位字比 Puffin 的
    // `Send a message` 长，tui-kit.md §4.0「旧界面独有」）。
    if caret {
        spans.push(Span::new(placeholder.to_owned(), theme.fg3));
    }
    on_row(single_line(spans, false), slot)
}

/// 板的底色：查看子代理时带一点 `agent`（tui-panels.md §1.6），否则 `panel`；plain 没有。
pub fn ground(app: &App) -> Option<Color> {
    if app.viewing.is_some() {
        app.ink.subagent_ground()
    } else {
        app.ink.theme.panel
    }
}

/// 输入框的板：rich 时一块底色（`▄` / `▀` 边，[`ground`]），plain 时上下两条细线（`rule` 缺省是 `line` 色，Puffin `composer`）。
pub fn board(app: &App, rows: Vec<Element>) -> Element {
    match ground(app) {
        Some(ground) => slab(stack(rows), ground),
        None => {
            let rule = rule(app).unwrap_or(app.ink.theme.line);
            stack(
                std::iter::once(hr('─', Color::Default).style(rule))
                    .chain(rows)
                    .chain(std::iter::once(hr('─', Color::Default).style(rule))),
            )
        }
    }
}

/// 草稿的一段文字怎么画：`inks` 是（起，止，样式）的范围（命令名、粘贴的占位），其余用终端的墨。
fn painted(d: &Draft<'_>, from: usize, to: usize) -> Vec<Span> {
    let (text, inks) = (d.text, d.inks);
    let mut out: Vec<Span> = Vec::new();
    let mut at = from;
    while at < to {
        let ink = inks.iter().find(|(a, b, _)| *a <= at && at < *b);
        let end = match ink {
            Some((_, b, _)) => (*b).min(to),
            None => inks
                .iter()
                .filter(|(a, _, _)| *a > at)
                .map(|(a, _, _)| *a)
                .min()
                .unwrap_or(to)
                .min(to),
        };
        let style = ink.map_or(Style::DEFAULT, |(_, _, s)| *s);
        let mut piece = text[at..end].replace('\t', " ");
        // 帽两边的方括号在 rich 主题里画成底色上的空格（Puffin 的帽 ` Pasted · 40 lines `）。
        if d.blanks.iter().any(|b| (at..end).contains(b)) {
            piece = text[at..end]
                .char_indices()
                .map(|(k, c)| if d.blanks.contains(&(at + k)) { ' ' } else { c })
                .collect();
        }
        match out.last_mut() {
            Some(last) if last.style == style => last.text.to_mut().push_str(&piece),
            _ => out.push(Span::new(piece, style)),
        }
        at = end;
    }
    out
}

/// 一段草稿：文字、折好的行（字节范围）、光标（字节；`None` 时不画）、墨色的范围、选区与光标处的幽灵提示。
pub struct Draft<'a> {
    pub text: &'a str,
    pub rows: &'a [(usize, usize)],
    pub cursor: Option<usize>,
    pub inks: &'a [(usize, usize, Style)],
    pub selection: Option<(usize, usize)>,
    pub hint: Option<&'a str>,
    /// 画成空格的字节位置（rich 主题里帽两边的方括号）。
    pub blanks: &'a [usize],
    /// 光标格里的粘贴的帽（字节范围）：整个帽反显，记作光标（Puffin `pastes::cap` 的 `under`）。
    pub under: Option<(usize, usize)>,
}

/// 草稿第 `index` 行（`row` 是它的字节范围）：领头是 `first`（第一行）或三格缩进；光标格是它下面的字（行末是一个空格）
/// 带 [`Style::CARET`]，或在末尾时是幽灵提示的第一个字（Puffin `draft_rows`）。
pub fn draft_row(theme: &Theme, d: &Draft<'_>, index: usize, first: &[Span]) -> StyledLine {
    let (start, end) = d.rows[index];
    let mut spans: Vec<Span> = if index == 0 {
        first.to_vec()
    } else {
        vec![Span::new(INDENT, Style::DEFAULT)]
    };
    let lead_width: usize = spans.iter().map(|s| string_width(&s.text)).sum();
    let on_row = d.cursor.filter(|c| {
        // 光标在软折行处属于下一行（Puffin §4.3「折行」第 4 条）：与 `TextArea::cursor_position` 同一规则。
        let last = d
            .rows
            .iter()
            .rposition(|(a, b)| *a <= *c && *c <= *b)
            .unwrap_or(0);
        last == index
    });
    match on_row {
        None => spans.extend(painted(d, start, end)),
        Some(cursor) if d.under.is_some_and(|(a, b)| a == cursor && b <= end) => {
            let (_, b) = d.under.unwrap_or_default();
            spans.extend(painted(d, start, cursor));
            let shown: String = d.text[cursor..b]
                .char_indices()
                .map(|(k, c)| {
                    if d.blanks.contains(&(cursor + k)) {
                        ' '
                    } else {
                        c
                    }
                })
                .collect();
            spans.push(Span::new(shown, Style::CARET));
            spans.extend(painted(d, b, end));
        }
        Some(cursor) => {
            spans.extend(painted(d, start, cursor));
            let next = d.text[cursor..end]
                .graphemes(true)
                .next()
                .map_or(cursor, |g| cursor + g.len());
            match d.hint.filter(|_| next == cursor) {
                Some(hint) => {
                    let ghost = theme.fg3.with_attr(attr::ITALIC);
                    let split = hint.chars().next().map_or(0, char::len_utf8);
                    spans.push(Span::new(
                        hint[..split].to_owned(),
                        ghost.with_attr(attr::REVERSE | attr::CARET),
                    ));
                    spans.push(Span::new(hint[split..].to_owned(), ghost));
                }
                None if next > cursor => {
                    spans.push(Span::new(
                        d.text[cursor..next].replace('\t', " "),
                        Style::CARET,
                    ));
                }
                None => spans.push(Span::new(" ", Style::CARET)),
            }
            spans.extend(painted(d, next, end));
        }
    }
    let mut line = StyledLine::new(spans);
    // 选区与这一行相交的部分反显（列从文字的第一列算起）。
    if let Some((from, to)) = d.selection {
        let (a, b) = (from.max(start), to.min(end));
        if a < b {
            let column = |at: usize| lead_width + string_width(&d.text[start..at]);
            line = line.restyle_cells(column(a), column(b), |style| style.with_attr(attr::REVERSE));
        }
    }
    line
}

// ---- 旧界面的输入框 ----

/// 板上的输入行：行（kit 的元素，最后一行带槽）、第一可见行与可见的行数。
pub struct Input {
    pub rows: Vec<Element>,
    pub first: usize,
    pub text_rows: usize,
}

/// 输入行第 1 列的记号：查看子代理时 `agent` 色的 `›`（tui-kit.md §4.3；它压过 bash 模式，tui-panels.md §1.6），bash 模式
/// 粗体的 `!`，进模式时从 brand 淡到 code（Puffin「Shell 命令」，kit `shell_block::mark`），否则 brand 粗体的 `›`。
fn mark(app: &App) -> Vec<Span> {
    let theme = &app.ink.theme;
    if app.viewing.is_some() {
        lead("›", Style::new(theme.agent, Color::Default, attr::BOLD))
    } else if app.picking.is_some() {
        // 挑选回退的点：brand 粗体的 `↶`（Puffin `chevron`）。
        lead("↶", Style::new(theme.brand, Color::Default, attr::BOLD))
    } else if app.composer.mode == InputMode::Bash {
        let age = app.bottom_motion.shell_age();
        lead(
            "!",
            rust_cli_tui_kit::app::widgets::shell_block::mark(theme, age),
        )
    } else {
        chevron(theme)
    }
}

/// plain 主题下板的两条细线的颜色：查看子代理 `agent`、搜索模式 brand（tui-kit.md §4.3），否则缺省的 `line`。
fn rule(app: &App) -> Option<Style> {
    if app.viewing.is_some() {
        Some(app.ink.subagent_ink())
    } else if app.port.find.is_some() {
        Some(Style::fg(app.ink.accent()))
    } else {
        None
    }
}

/// 标题行右边写的字（tui-tasks.md §2.3）：查看子代理时它的描述（`agent` 墨，不走开），浏览输入历史时 `History k/n`，否则
/// 会话标题（fg3）；没有字时没有。
pub fn title(app: &App) -> Option<Title> {
    let fg3 = app.ink.theme.fg3;
    let (text, style, stays) = if let Some(viewing) = &app.viewing {
        let words: Vec<&str> = viewing.description.split_whitespace().collect();
        (words.join(" "), app.ink.subagent_ink(), true)
    } else if let Some((index, total)) = app.composer.history_position() {
        (format!("History {index}/{total}"), fg3, false)
    } else {
        (app.view.title().trim().to_owned(), fg3, false)
    };
    (!text.is_empty()).then_some(Title { text, style, stays })
}

/// 开头的命令名（`/名字` 后面是空白或结尾，名字是已知的命令或 skill）的字节长度（Puffin `command_name`）。
fn command_end(app: &App, text: &str) -> Option<usize> {
    let rest = text.strip_prefix('/')?;
    let name = rest.split(char::is_whitespace).next().unwrap_or("");
    let known = !name.is_empty()
        && (super::commands::find(name).is_some()
            || super::commands::skill(&app.composer.skills, name).is_some());
    known.then_some(1 + name.len())
}

/// 只输入了带参数的命令名（`/名称 `）时的参数提示：TUI 命令的提示，skill 用 frontmatter 的 `argument-hint`（tui.md §6）。
fn argument_hint<'a>(app: &'a App, text: &str) -> Option<&'a str> {
    let name = text.strip_prefix('/')?.strip_suffix(' ')?;
    // `/rename ` 的 ghost 是会话现在的名字，⇥ 接过来（Puffin `rename_ghost`）；没有名字时照旧是参数提示。
    if let Some(title) = rename_ghost(app, text) {
        return Some(title);
    }
    // `/init` 是内核的 skill；提示照 Puffin（`consts::hint`）。
    if name.eq_ignore_ascii_case("init") {
        return Some("notes");
    }
    super::commands::find(name)
        .and_then(super::commands::Command::arg_hint)
        .or_else(|| {
            super::commands::skill(&app.composer.skills, name)
                .and_then(|skill| skill.argument_hint.as_deref())
        })
}

/// 草稿正好是 `/rename `（或它的别名）时会话现在的名字（Puffin `naming::rename_ghost`，名字非空才有）。
pub(super) fn rename_ghost<'a>(app: &'a App, text: &str) -> Option<&'a str> {
    let name = text.strip_prefix('/')?.strip_suffix(' ')?;
    if name.is_empty() || name.contains(char::is_whitespace) {
        return None;
    }
    let renames = super::commands::find(&name.to_lowercase()).is_some_and(|c| c.name == "rename");
    Some(app.view.title()).filter(|t| renames && !t.is_empty())
}

/// 图片的占位文字的墨：rich 时 Puffin 键帽的样子（`line` 的底、fg2），plain 时 dim（tui-kit.md §6：图片的占位仍是旧的
/// `[Image #N]`）。
fn chip_style(theme: &Theme) -> Style {
    if theme.rich {
        theme.fg2.with_bg(theme.line.fg_color())
    } else {
        theme.fg3
    }
}

/// 粘贴的帽 `[Pasted · 40 lines]` 在 `start..end` 的墨（kit `paste_caps::cap` 落定的样子）：rich 时整块在 `line` 的底上，
/// `Pasted` 与大小 fg2、点 fg3，两边的方括号画成空格（[`Draft::blanks`]）；plain 时方括号 fg3，标签是正文的墨。
fn cap_inks(theme: &Theme, text: &str, start: usize, end: usize) -> Vec<(usize, usize, Style)> {
    let dot = text[start..end].find(" · ").map_or(end, |k| start + k);
    if !theme.rich {
        return vec![
            (start, start + 1, theme.fg3),
            (start + 1, end - 1, Style::DEFAULT),
            (end - 1, end, theme.fg3),
        ];
    }
    let ground = theme.line.fg_color();
    let mid = (dot + " · ".len()).min(end);
    vec![
        (start, dot, theme.fg2.with_bg(ground)),
        (dot, mid, theme.fg3.with_bg(ground)),
        (mid, end, theme.fg2.with_bg(ground)),
    ]
}

/// 墨色范围按 kit `fade::fade_in` 从板上淡入了 `age` 个 tick（只换墨色，范围不变）。
fn faded_inks(
    theme: &Theme,
    inks: Vec<(usize, usize, Style)>,
    age: i64,
) -> Vec<(usize, usize, Style)> {
    use rust_cli_tui_kit::app::widgets::fade;
    let spans = inks.iter().map(|(_, _, s)| Span::new("", *s)).collect();
    let faded = fade::fade_in(theme, spans, age, fade::TICKS, theme.panel);
    inks.into_iter()
        .zip(faded)
        .map(|((a, b, _), span)| (a, b, span.style))
        .collect()
}

/// 草稿里每个粘贴的帽的字节范围（按位置）。
fn caps(app: &App, text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for placeholder in app.composer.paste_placeholders() {
        let mut from = 0;
        while let Some(at) = text[from..].find(placeholder) {
            let start = from + at;
            out.push((start, start + placeholder.len()));
            from = start + placeholder.len();
        }
    }
    out.sort_unstable();
    out
}

/// 草稿的墨色范围：开头的命令名 brand，图片的占位是键帽的样子，粘贴是帽（[`cap_inks`]）。
fn inks(app: &App, text: &str) -> Vec<(usize, usize, Style)> {
    let theme = &app.ink.theme;
    let mut out = Vec::new();
    if app.composer.mode != InputMode::Bash
        && let Some(end) = command_end(app, text)
    {
        out.push((0, end, Style::fg(theme.brand)));
    }
    // 认识的 `$技能` 是技能的墨色，刚插入的从板上淡入（`skill_view.rs`）；bash 模式下 `$` 是 shell 的。
    if app.composer.mode != InputMode::Bash {
        out.extend(app.skill_inks(text, app.status_motion.tick()));
        // `@` 面板插入的会话：整块的提及，会话的墨色（`skill_view::mark_inks`）。
        out.extend(app.mark_inks(app.status_motion.tick()));
        // 插入的范围里其余的字（加的空格）一起淡入（Puffin `paint`）。
        let gaps = app.inserted_gaps(&out, app.status_motion.tick());
        out.extend(gaps);
    }
    let chip = chip_style(theme);
    // 帽的底色与标签作为一整块从板上淡入（Puffin `pastes::cap` 的 `born`，input_motion.rs）。
    for (start, end) in caps(app, text) {
        let age = app.bottom_motion.cap_age(&text[start..end]);
        out.extend(faded_inks(theme, cap_inks(theme, text, start, end), age));
    }
    for placeholder in app.composer.image_placeholders() {
        let mut from = 0;
        while let Some(at) = text[from..].find(placeholder) {
            let start = from + at;
            out.push((start, start + placeholder.len(), chip));
            from = start + placeholder.len();
        }
    }
    out.sort_by_key(|(a, _, _)| *a);
    out
}

/// 输入框的输入行。搜索历史时显示匹配的那条（查询出现处 `code` 色），槽里是 `search prompts: 查询` 与光标；搜索模式见
/// `find::input_row`。`slot` 是槽的段（放得下时在最后一行的右端）；`caret` 为假时不画光标（请求或页面拿着键，草稿行留着，
/// tui-kit.md §4.0「板」）。
/// `answer`：草稿空着时代替占位句的一句（请求的回答在输入框里打时，`Laid::answer`）。
pub fn input_rows(
    app: &App,
    width: usize,
    max_rows: usize,
    slot: Vec<Span>,
    caret: bool,
    answer: Option<&str>,
) -> Input {
    let theme = &app.ink.theme;
    let area = &app.composer.area;
    let lead = mark(app);
    if let Some((query, _)) = &app.composer.search {
        return Input {
            rows: vec![search_row(app, query, lead, width)],
            first: 0,
            text_rows: 1,
        };
    }
    if area.is_empty() {
        return Input {
            rows: vec![placeholder_row_with(
                theme,
                lead,
                &answer.map_or_else(|| placeholder(app), str::to_owned),
                slot,
                caret,
            )],
            first: 0,
            text_rows: 1,
        };
    }
    let text_width = text_width(width);
    let text = area.text();
    let rows: Vec<(usize, usize)> = area
        .rows(text_width)
        .iter()
        .map(|row| (row.start, row.end))
        .collect();
    let (cursor_row, _) = area.cursor_position(text_width);
    let first = rows.len().saturating_sub(max_rows).min(cursor_row);
    let inks = inks(app, text);
    // 跟着草稿的列表开着、或有待回答的请求时不写参数提示（Puffin：选择器、模式、导出、主题、回退与 `/resume` 的列表开着时没有）。
    let listing = matches!(
        app.overlay,
        Some(
            super::app::Overlay::List(_)
                | super::app::Overlay::Sessions(_)
                | super::app::Overlay::Points(_)
                | super::app::Overlay::Forks(_)
        )
    );
    let hint = (area.cursor() == text.len() && !listing && app.pending().is_none())
        .then(|| argument_hint(app, text))
        .flatten();
    let caps = caps(app, text);
    let blanks: Vec<usize> = if theme.rich {
        caps.iter().flat_map(|(a, b)| [*a, b - 1]).collect()
    } else {
        Vec::new()
    };
    let under = caps
        .iter()
        .copied()
        .find(|(a, _)| caret && *a == area.cursor() && area.selection().is_none());
    let draft = Draft {
        text,
        rows: &rows,
        cursor: caret.then(|| area.cursor()),
        inks: &inks,
        selection: area.selection(),
        hint,
        blanks: &blanks,
        under,
    };
    let shown = rows.len().min(first + max_rows);
    let lines: Vec<Element> = (first..shown)
        .map(|index| {
            let line = single_line(draft_row(theme, &draft, index, &lead).spans, false);
            if index + 1 == shown {
                on_row(line, slot.clone())
            } else {
                line
            }
        })
        .collect();
    Input {
        text_rows: lines.len(),
        rows: lines,
        first,
    }
}

/// 搜索历史（ctrl+r）的输入行：匹配到的那条（查询出现的地方 `code` 色，截到给槽留出的宽度），槽里是 `search prompts:` 与
/// 查询，光标在查询末尾（tui-kit.md §4.3）。
fn search_row(app: &App, query: &str, lead: Vec<Span>, width: usize) -> Element {
    let theme = &app.ink.theme;
    let missing = !query.is_empty() && app.composer.search_match().is_none();
    let label = match (app.lang, missing) {
        (Lang::En, false) => "search prompts: ",
        (Lang::En, true) => "no matching prompt: ",
        (Lang::Zh, false) => "搜索输入：",
        (Lang::Zh, true) => "没有匹配的输入：",
    };
    let slot = vec![
        Span::new(label, theme.fg3),
        Span::new(query.to_owned(), Style::DEFAULT),
        Span::new(" ", Style::CARET),
    ];
    let slot_width: usize = slot.iter().map(|s| string_width(&s.text)).sum();
    let shown = app
        .composer
        .search_match()
        .unwrap_or(app.composer.area.text());
    let shown = shown.replace('\n', " ");
    let room = edge(width).saturating_sub(TEXT_COL + 2 + slot_width);
    let shown = truncate(&shown, room);
    let mut spans = lead;
    let mut rest = shown.as_str();
    while let Some(at) = (!query.is_empty()).then(|| rest.find(query)).flatten() {
        spans.push(Span::new(rest[..at].to_owned(), Style::DEFAULT));
        spans.push(Span::new(query.to_owned(), Style::fg(theme.code)));
        rest = &rest[at + query.len()..];
    }
    spans.push(Span::new(rest.to_owned(), Style::DEFAULT));
    let used: usize = spans.iter().map(|s| string_width(&s.text)).sum();
    let end = edge(width);
    if used + slot_width < end {
        spans.push(Span::new(
            " ".repeat(end - used - slot_width),
            Style::DEFAULT,
        ));
    } else {
        spans.push(Span::new("  ", Style::DEFAULT));
    }
    spans.extend(slot);
    single_line(spans, false)
}

/// 粘贴的预览开着：光标在一个粘贴的帽上，没有选区、没有在搜索历史、没有别的层（Puffin `peek_open`）。
pub fn peek_open(app: &App) -> bool {
    app.composer.paste_under_cursor().is_some()
        && app.composer.area.selection().is_none()
        && app.composer.search.is_none()
        && app.composer.popup.is_none()
        && app.overlay.is_none()
}

/// 光标在粘贴的帽上时它的预览（Puffin `peek_open`，kit `paste_caps::text_peek`）：板上、输入行之上，`│` 轨上的前五行与还有
/// 多少，打开时一起淡入（Puffin `peek_at`，input_motion.rs）。
pub fn paste_peek(app: &App, width: usize) -> Vec<(Element, Option<usize>)> {
    let Some((_, body)) = app.composer.paste_under_cursor().filter(|_| peek_open(app)) else {
        return Vec::new();
    };
    rust_cli_tui_kit::app::widgets::paste_caps::text_peek(
        &app.ink.theme,
        body.trim_end_matches('\n'),
        width,
        app.bottom_motion.peek_age(),
    )
    .into_iter()
    .map(|row| (row, None))
    .collect()
}

#[cfg(test)]
#[path = "input_box_tests.rs"]
mod tests;
