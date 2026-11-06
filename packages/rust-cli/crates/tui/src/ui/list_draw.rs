//! 选择列表的画法（docs/specs/rust-cli/tui-kit.md §4.2「`/model`、`/mode`、`/effort` 等列表」与「`/theme`」，工作线 TK
//! 的 S3）：照 Puffin 输入框的板上的那一层画（docs/puffin-tui.md §4.2「模型选择器」「权限模式」「主题」，壳的
//! `shell/picker.rs`、`shell/modes.rs`、`shell/theme_list.rs`）。行为（条目、按键、数字直选、过滤）在 `list.rs`，不变。
//!
//! 一层是：头一行（标题，单行的副标题放得下时三格后接在它后面，同 Puffin `/theme` 的头；否则副标题各占一行 fg3），条目的
//! 窗口（`↑ n more` / `↓ n more`），选中条目的细节行（模型的档位行、rewind 的改动），可过滤时最后是输入行（` › 查询`）。
//! 整层在板上（`pick::layer`），板下面一行是按键提示（Puffin 状态栏的槽，第 3 列起）。
//!
//! 与旧画法的不同（tui-kit.md §6 登记）：没有顶上的强调色 `─` 线、`› ` 指针与底色条、`1.` 的序号（数字键照旧直选）、绿色
//! 的 ` ✔`（改为 fg3 的 `current`）；`/mode` 每个模式一行（名字、说明），名字不按模式着色；窗口最多 7 行（`/theme` 9 行）。
use rust_cli_tui_kit::app::element::Element;
use rust_cli_tui_kit::app::widgets::model_picker::effort_bars_said;
use rust_cli_tui_kit::render::width::string_width;
use rust_cli_tui_kit::render::{Span, StyledLine};

use super::{ListView, Purpose};
use crate::ui::i18n::Lang;
use crate::ui::ink::Ink;
use crate::ui::pick::{self, WINDOW, chosen, desc_style, name_style, pad, row};

/// `/theme` 列表的窗口（Puffin `theme_list::ROWS`）。
pub const THEME_WINDOW: usize = 9;
/// 名字列最宽算到这么多格（再多的名字被说明顶出去）。
const NAME_MAX: usize = 40;
/// 窄于这么多列时模型列表没有 provider 列（Puffin `PICKER_NARROW`）。
const PICKER_NARROW: usize = 64;
/// provider 名最多这么多格。Puffin 的 `PROVIDER_MAX` 是 20；旧界面放宽到 40（tui-kit.md §6）：团队套餐的名字
/// （`BigModel Team Plan · 团队 A`）截到 20 格后各团队一模一样，选不出要的团队。
const PROVIDER_MAX: usize = 40;

/// 名字一列、说明一列的一行（Puffin `/mode`、`/export`、`/theme` 的行）：名字补到 `name_width`，`tag` 是 fg3 的
/// `   current` / `   on` 一类的尾巴。
pub fn named_row(
    ink: &Ink,
    label: &str,
    desc: &str,
    tag: Option<&str>,
    selected: bool,
    name_width: usize,
) -> Element {
    let mut spans = vec![Span::new(pad(label, name_width), name_style(ink, selected))];
    if !desc.is_empty() {
        spans.push(Span::new(desc.to_owned(), desc_style(ink, selected)));
    }
    if let Some(tag) = tag.filter(|tag| !tag.is_empty()) {
        spans.push(Span::new(format!("   {tag}"), ink.theme.fg3));
    }
    row(spans)
}

/// 模型选择器的一个模型（Puffin `picker_rows` 的数据）。
pub struct ModelRow<'a> {
    pub provider: &'a str,
    pub id: &'a str,
    pub current: bool,
}

/// 模型选择器的行（Puffin `picker_rows`）：选中的周围最多 `window` 行的模型，然后是档位行（kit `effort_bars_said`：档位的名字
/// 换了 `efforts.2` tick 之后从板上淡入）。`widest_provider` 是整个目录里最宽的 provider 名；`current` 是 `current` 一词；
/// `effort` 是档位行的标签。
#[allow(clippy::too_many_arguments, reason = "Puffin 的选择器的全部输入")]
pub fn model_rows(
    ink: &Ink,
    models: &[ModelRow<'_>],
    selected: usize,
    filter: &str,
    widest_provider: usize,
    width: usize,
    window: usize,
    words: ModelWords<'_>,
    efforts: (&[String], Option<usize>, i64),
) -> Vec<Element> {
    let theme = &ink.theme;
    let pw = if width < PICKER_NARROW {
        0
    } else {
        widest_provider.min(PROVIDER_MAX) + 3
    };
    let nw = models.iter().map(|m| string_width(m.id)).max().unwrap_or(0) + 3;
    let sel = selected.min(models.len().saturating_sub(1));
    let (from, n) = pick::window(sel, models.len(), window);
    // Puffin 的 `more` 总在前面放 provider 列宽的空格（窄时是空段）。
    let more = |text: String| {
        row(vec![
            Span::plain(" ".repeat(pw)),
            Span::new(text, theme.fg3),
        ])
    };
    let mut rows = Vec::new();
    if from > 0 {
        rows.push(more(format!("↑ {from} more")));
    }
    for i in from..from + n {
        let m = &models[i];
        let group = i == from || models[i - 1].provider != m.provider;
        let mut spans = Vec::new();
        if pw > 0 {
            let name = if group {
                pick::clip(m.provider, PROVIDER_MAX)
            } else {
                String::new()
            };
            spans.push(Span::new(pad(&name, pw), theme.fg3));
        }
        if i == sel {
            spans.push(Span::new(m.id.to_owned(), chosen(ink)));
        } else {
            spans.extend(pick::matched(ink, m.id, filter));
        }
        if m.current {
            spans.push(Span::plain(
                " ".repeat(nw.saturating_sub(string_width(m.id))),
            ));
            spans.push(Span::new(words.current.to_owned(), theme.fg3));
        }
        rows.push(row(spans));
    }
    if from + n < models.len() {
        rows.push(more(format!("↓ {} more", models.len() - from - n)));
    }
    let label = if pw == 0 {
        format!("{} ", words.effort)
    } else {
        pad(words.effort, pw - 1)
    };
    let mut effort = vec![Span::new(label, theme.fg3)];
    effort.extend(effort_bars_said(
        theme,
        efforts.0,
        efforts.1,
        efforts.2,
        words.no_effort,
    ));
    rows.push(row(effort));
    rows
}

/// 模型选择器里的词（中英文）。
#[derive(Clone, Copy)]
pub struct ModelWords<'a> {
    pub current: &'a str,
    pub effort: &'a str,
    pub no_effort: &'a str,
}

impl ModelWords<'static> {
    pub fn of(lang: Lang) -> Self {
        match lang {
            Lang::En => ModelWords {
                current: "current",
                effort: "Effort",
                no_effort: "No effort setting for this model.",
            },
            Lang::Zh => ModelWords {
                current: "当前",
                effort: "推理档位",
                no_effort: "这个模型没有推理档位。",
            },
        }
    }
}

/// 头一行与副标题：单行的副标题放得下时接在标题后面（Puffin `/theme` 的头），否则副标题的每一行在下面按词折行。
fn head_rows(list: &ListView, ink: &Ink, width: usize) -> Vec<Element> {
    let room = width
        .saturating_sub(pick::MEASURE_MARGIN)
        .min(pick::MEASURE_MAX)
        .saturating_sub(pick::INDENT.len());
    let lines: Vec<&str> = list
        .subtitle
        .as_deref()
        .map(|s| s.lines().collect())
        .unwrap_or_default();
    match lines.as_slice() {
        // 主题的头一行照 Puffin（kit `theme_list::rows`）：总是一行，放不下时用 `…` 截断，不折行。
        [one] if list.purpose == Purpose::Theme => {
            vec![pick::head(&list.title, Some(one), ink)]
        }
        [one] if string_width(&list.title) + 3 + string_width(one) <= room => {
            vec![pick::head(&list.title, Some(one), ink)]
        }
        _ => std::iter::once(pick::head(&list.title, None, ink))
            .chain(lines.iter().map(|l| pick::quiet_wrap(*l, ink)))
            .collect(),
    }
}

impl ListView {
    /// 视图的行与每个条目所在的行（行号，`visible()` 里的下标）。窗口不看屏幕的高度（Puffin 的层）：比底部栈能占的高时由
    /// `splice` 从上面截。以前矮的终端里窗口缩小，至少 3 项。
    pub fn lines(
        &self,
        width: usize,
        lang: Lang,
        ink: &Ink,
    ) -> (Vec<StyledLine>, Vec<(usize, usize)>) {
        let zh = lang == Lang::Zh;
        // 模型、模式与回退的列表没有头一行（Puffin 的这几层没有，tui-kit.md §4.0「板」）；主题、记忆、fork 与确认留着。
        let headless = matches!(
            self.purpose,
            Purpose::Model | Purpose::Permissions | Purpose::Export
        );
        let mut els = if headless {
            Vec::new()
        } else {
            head_rows(self, ink, width)
        };
        // 副标题可能折成几行：条目的行号从头几行之后数起（板由 `pane.rs` 画，行号相对这一层的第一行）。
        let head = pick::lay(&els, width).len();
        let visible = self.visible();
        let focused = visible.get(self.selected).copied();
        let window = if self.purpose == Purpose::Theme {
            THEME_WINDOW
        } else {
            WINDOW
        };
        let mut rows = Vec::new();
        if visible.is_empty() {
            // 有条目但搜索没有匹配：不用列表为空时的说法（模型的是“去登录”）。
            let empty = match (self.items.is_empty(), zh) {
                (true, _) => self.empty.as_str(),
                (false, false) => "No matches",
                (false, true) => "没有匹配",
            };
            els.push(pick::quiet_row(empty, ink));
        } else if self.purpose == Purpose::Model {
            let models: Vec<ModelRow<'_>> = visible
                .iter()
                .map(|item| ModelRow {
                    provider: &item.description,
                    id: &item.label,
                    current: item.current,
                })
                .collect();
            let widest = self
                .items
                .iter()
                .map(|item| string_width(&item.description))
                .max()
                .unwrap_or(0);
            let levels = focused.map_or(&[][..], |item| &item.levels[..]);
            let level = focused
                .filter(|item| !item.levels.is_empty())
                .map(|item| item.level.min(item.levels.len() - 1));
            let (from, n) = pick::window(self.selected, models.len(), window);
            let start = head + usize::from(from > 0);
            rows.extend((from..from + n).map(|i| (start + i - from, i)));
            els.extend(model_rows(
                ink,
                &models,
                self.selected,
                self.filter.as_deref().unwrap_or(""),
                widest,
                width,
                window,
                ModelWords::of(lang),
                (levels, level, self.named_age),
            ));
        } else {
            let (from, until) = if self.purpose == Purpose::Theme {
                theme_window(visible.len(), self.selected, window)
            } else {
                let (from, n) = pick::window(self.selected, visible.len(), window);
                (from, from + n)
            };
            let head_count = els.len();
            if from > 0 {
                els.push(pick::more_above(from, 0, ink));
            }
            // `/mode` 与 `/export` 的名字补到 21 格（Puffin `pad(label, 21)`），别的按最长的名字。
            let name_width = if matches!(self.purpose, Purpose::Permissions | Purpose::Export) {
                21
            } else {
                self.items
                    .iter()
                    .map(|item| string_width(&item.label))
                    .max()
                    .unwrap_or(0)
                    .min(NAME_MAX)
                    + 3
            };
            let current = if zh { "当前" } else { "current" };
            for (index, item) in visible.iter().enumerate().take(until).skip(from) {
                let selected = index == self.selected;
                rows.push((head + els.len() - head_count, index));
                let desc = if width >= 24 {
                    &item.description[..]
                } else {
                    ""
                };
                let tag = if item.tag.is_empty() {
                    item.current.then_some(current)
                } else {
                    Some(item.tag.as_str())
                };
                els.push(named_row(ink, &item.label, desc, tag, selected, name_width));
            }
            if until < visible.len() {
                els.push(pick::more_below(visible.len() - until, 0, ink));
            }
        }
        // 过滤的词在草稿里（`/model 词`，tui-kit.md §4.0），这一层没有自己的输入行；按键提示在槽里（`pane.rs`）。
        let lines = pick::layer(els, ink, width);
        (lines, rows)
    }
}

/// `/theme` 的窗口（Puffin `consts::shelf_window`）：`n` 个里选中 `choice`、最多 `rows` 行时显示的 [from, until)。
pub fn theme_window(n: usize, choice: usize, rows: usize) -> (usize, usize) {
    let rows = rows.max(3);
    if n <= rows {
        (0, n)
    } else if choice + 1 < rows {
        (0, rows - 1)
    } else if choice + rows > n {
        (n + 1 - rows, n)
    } else {
        let from = choice + 3 - rows;
        (from, from + rows - 2)
    }
}
