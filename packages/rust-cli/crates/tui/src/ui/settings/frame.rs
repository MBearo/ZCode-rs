//! 设置页的整屏（docs/specs/rust-cli/tui-kit.md §4.2「设置」；照 Puffin `shell/settings/slab.rs` 的 `settings_lines`、
//! `settings_slab`、`settings_input`、`default_picker_rows` 与 `view.rs`、`hints.rs`）：页代替正文，输入框（搜索与键入的值）
//! 与状态栏留在底下，状态栏右边是这一页的键，没有数字的那组。从输入框长出来的行（值的名字、缺省模型的选择器）盖住页的
//! 最后几行，页不动。画法全是 kit 的 `widgets::settings`；这里把旧界面的状态映射成它的中性入参。
use std::collections::BTreeMap;
use std::time::Instant;

use rust_cli_kernel_api::orchestration::InteractionRequest;
use rust_cli_kernel_api::provider::CatalogModel;
use rust_cli_tui_kit::app::element::{
    ClipFocus, Element, clip_rows, measure, single_line, slab, stack,
};
use rust_cli_tui_kit::app::frame::PROMPT_ARROW;
use rust_cli_tui_kit::app::layout::Layout;
use rust_cli_tui_kit::app::widgets::fade;
use rust_cli_tui_kit::app::widgets::fit::{MEASURE_MARGIN, MEASURE_MAX, clip, pad};
use rust_cli_tui_kit::app::widgets::lists::picker_window;
use rust_cli_tui_kit::app::widgets::model_picker::{PICKER_NARROW, PROVIDER_MAX, effort_bars};
use rust_cli_tui_kit::app::widgets::playhead::TICK_MS;
use rust_cli_tui_kit::app::widgets::queue_stack::COMPOSER_INDENT;
use rust_cli_tui_kit::app::widgets::settings::hints::{self, Asking, Chosen, Enter, Keys, Space};
use rust_cli_tui_kit::app::widgets::settings::view::{
    self as kit, Mode as KitMode, PageCtx, Search,
};
use rust_cli_tui_kit::app::widgets::settings::{NARROW, Row as KitRow, chosen_style};
use rust_cli_tui_kit::markdown::render::INDENT;
use rust_cli_tui_kit::render::width::string_width;
use rust_cli_tui_kit::render::{Span, Style, StyledLine, attr};
use rust_cli_tui_kit::theme::theme::Theme;

use super::field::Field;
use super::keys::effort_of;
use super::page::{Act, Page, Row, View};
use super::{Mode, Settings, row_key};
use crate::ui::app::App;

/// 值的行至多这么高（Puffin `PROMPT_BOX_MAX_ROWS`）。
const INPUT_MAX_ROWS: usize = 8;

fn row(spans: Vec<Span>) -> Element {
    let mut all = vec![Span::new(INDENT, Style::DEFAULT)];
    all.extend(spans);
    measure(single_line(all, false), MEASURE_MAX, MEASURE_MARGIN)
}

/// 请求问的，输入框上方琥珀色的那一行。
fn asking(app: &App) -> Option<&'static str> {
    let (_, request) = app.pending()?;
    Some(hints::asking_line(match request {
        InteractionRequest::Question { .. } => Asking::Question,
        InteractionRequest::PlanApproval { .. } => Asking::Plan,
        _ => Asking::Permission,
    }))
}

/// 选中的行在状态栏的说法。
fn chosen_of(row: &Row) -> Chosen<'_> {
    match row {
        Row::Switch { on, .. } => Chosen::Switch { on: *on },
        Row::Item(it) => Chosen::Item {
            space: match (it.toggle, &it.flip) {
                // 机器的空格随状态连接、断开或停下。
                (Some(_), Some(Act::Machine(a))) => Space::Word(a.word()),
                (Some(on), Some(_)) => Space::Toggle(on),
                _ => Space::None,
            },
            remove: it.remove.is_some(),
            open: it.open.as_ref().map(|_| it.enter.as_str()),
        },
        Row::Text(_) => Chosen::Text,
        Row::Editor { .. } => Chosen::Editor,
        Row::Action { off: true, .. } => Chosen::Action(Enter::Off),
        Row::Action {
            act: Act::Machine(a),
            ..
        } => Chosen::Action(Enter::Word(a.word())),
        Row::Action {
            act: Act::SetForm { .. },
            plus: true,
            ..
        } => Chosen::Action(Enter::UseIt),
        Row::Action {
            act: Act::Push(Page::NewRule),
            ..
        } => Chosen::Action(Enter::AddRule),
        Row::Action { danger, plus, .. } => Chosen::Action(if *danger {
            Enter::Delete
        } else if *plus {
            Enter::Add
        } else {
            Enter::Run
        }),
        _ => Chosen::Other,
    }
}

/// 页的状态下状态栏的键。
pub fn keys(
    ui: &Settings,
    chosen: Option<&Row>,
    asking: bool,
    narrow: bool,
) -> Vec<(String, String)> {
    let mode = match &ui.mode {
        // 草稿有错（规则少了结尾的 `)`）时没有 ⏎。
        Mode::Editing { .. } => Keys::Editing {
            stuck: matches!(chosen, Some(Row::Text(t)) if t.stuck),
        },
        Mode::Choosing { .. } => Keys::Choosing,
        Mode::Confirming { .. } => Keys::Confirming,
        Mode::Picking { .. } => Keys::Picking,
        Mode::Browse if ui.searching() => Keys::Searching,
        Mode::Browse if ui.list_focused(narrow) => Keys::Directory,
        Mode::Browse => Keys::Page {
            stacked: !ui.stack.is_empty(),
        },
    };
    hints::keys(mode, chosen.map(chosen_of), asking, &ui.editor, narrow)
}

/// 页的状态在 kit 里的样子：在做的（按行的下标）与每一行的值变了多久（kit 的 tick）。
fn with_ctx<R>(
    ui: &Settings,
    view: &View,
    theme: &Theme,
    now: Instant,
    f: impl FnOnce(&[KitRow<'_>], &PageCtx<'_>) -> R,
) -> R {
    let rows: Vec<_> = view.rows.iter().map(Row::kit).collect();
    let mode = match &ui.mode {
        Mode::Choosing { key, at } => view
            .rows
            .iter()
            .position(|r| matches!(r, Row::Choice { key: ck, .. } if ck == key))
            .map_or(KitMode::Browse, |row| KitMode::Choosing { row, at: *at }),
        Mode::Confirming {
            key,
            at,
            label,
            note,
            ..
        } => view
            .rows
            .iter()
            .position(|r| r.id() == *key)
            .map_or(KitMode::Browse, |row| KitMode::Confirming {
                row,
                at: *at,
                label,
                note,
            }),
        _ => KitMode::Browse,
    };
    let test = ui.test.as_ref().and_then(|(k, result)| {
        let row = view
            .rows
            .iter()
            .position(|r| matches!(r, Row::Action { key, .. } if key == k))?;
        Some((
            row,
            result
                .as_ref()
                .map(|r| r.as_ref().map(|ms| *ms).map_err(String::as_str)),
        ))
    });
    let page = ui.page();
    let age = |i: usize| {
        view.rows.get(i).map_or(-1, |r| {
            ui.typed
                .get(&row_key(&page, &r.id()))
                .copied()
                .flatten()
                .map_or(-1, |t| {
                    let ms = now.saturating_duration_since(t).as_millis();
                    (ms / u128::from(TICK_MS)).min(i64::MAX as u128) as i64
                })
        })
    };
    let ctx = PageCtx {
        theme,
        editor: &ui.editor,
        mode,
        sign_in: None,
        test,
        age: &age,
    };
    f(&rows, &ctx)
}

impl App {
    /// 设置页开着时的整屏，`rows` 行；没开时 `None`。`draw.rs` 先问这里（Puffin `frame_of` 里设置页在 transcript 之前）。
    pub fn settings_frame(
        &mut self,
        now: Instant,
        now_ms: u64,
        width: usize,
        rows: usize,
    ) -> Option<Vec<StyledLine>> {
        let ui = self.settings.as_mut()?;
        ui.width = width;
        for t in ui.typed.values_mut() {
            t.get_or_insert(now);
        }
        // 缺陷修复（孪生 settings-general 第 31 步）：Puffin 的槽不看设置页（`slot_of` 照草稿算，页上不画），换了的槽在页开着
        // 时悄悄淡完，关上时 `? keys` 直接在。以前设置页不记槽，关上时槽从打开前的样子换过来、从头淡入。
        let says: String = super::super::footer::slot(self)
            .spans
            .iter()
            .map(|s| s.text.as_ref())
            .collect();
        if self.slot_seen.as_ref().is_none_or(|(was, _)| *was != says) {
            self.slot_seen = Some((says, now));
        }
        let ui = self.settings.clone()?;
        self.pointer.selection = None;
        self.pointer.screen_selection = None;
        self.pointer.hits = Default::default();
        let width = width.max(1);
        let narrow = width < NARROW;
        let rows = rows.max(1);
        let theme = self.ink.theme;
        let view = self.settings_view(&ui, now_ms);
        let chosen_row = self.settings_chosen(&ui, now_ms);
        // 目录有焦点时页里没有选中的行。
        let chosen = if ui.list_focused(narrow) {
            None
        } else {
            chosen_row.as_ref().map(|(i, _)| *i)
        };
        let asking_what = asking(self);
        let (grown, input) = self.settings_slab(&ui, &theme, width);
        let over = Layout::lay(&stack(grown.clone()), width).len();
        let mut slab_rows = grown;
        slab_rows.push(input);
        let boxed = match theme.panel {
            Some(p) => slab(stack(slab_rows), p),
            None => {
                let mut all = vec![Element::Rule('─', theme.line)];
                all.extend(slab_rows);
                all.push(Element::Rule('─', theme.line));
                stack(all)
            }
        };
        let said = ui
            .said
            .as_ref()
            .map(|r| r.as_ref().map(String::as_str).map_err(String::as_str));
        let mut above = hints::said(&theme, said, asking_what);
        above.push(boxed);
        let mut bottom = Layout::lay(&stack(above), width);
        let pairs = keys(
            &ui,
            chosen_row.as_ref().map(|(_, r)| r),
            asking_what.is_some(),
            narrow,
        );
        // 刚按 ⌃y 复制了这一页：按键提示的位置说一句，到下一个键（page_copy.rs）。
        let note = if self.page_copied.is_some() {
            super::super::page_copy::said(self)
        } else {
            rust_cli_tui_kit::app::widgets::key_hints::spans(&theme, &pairs)
        };
        bottom.extend(super::super::footer::settings_status(
            self, width, rows, note,
        ));
        let counts = ui.searching().then(|| {
            let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
            for (s, _) in self.settings_hits(&ui, now_ms) {
                *counts.entry(s).or_default() += 1;
            }
            counts
        });
        let body_h = rows.saturating_sub(bottom.len().saturating_sub(over));
        let body = with_ctx(&ui, &view, &theme, now, |kit_rows, ctx| {
            let page = kit::Page {
                section: ui.section,
                crumbs: &view.crumbs,
                desc: &view.desc,
                rows: kit_rows,
            };
            let search = counts.as_ref().map(|hits| Search {
                word: &ui.search.text,
                hits,
            });
            kit::body(
                &page,
                chosen,
                search,
                width,
                body_h,
                ctx,
                ui.list_focused(narrow),
            )
        });
        let mut lines: Vec<StyledLine> = body
            .into_iter()
            .take(rows.saturating_sub(bottom.len()))
            .collect();
        // ⌃y 复制的是这一屏的页（page_copy.rs）。
        self.settings_shown = lines.clone();
        lines.extend(bottom);
        let skip = lines.len().saturating_sub(rows);
        Some(lines.split_off(skip))
    }

    /// 设置页的输入框：长在它上面的行（值的名字、缺省模型的选择器）与输入行本身。
    fn settings_slab(&self, ui: &Settings, theme: &Theme, width: usize) -> (Vec<Element>, Element) {
        match &ui.mode {
            Mode::Editing { key, draft } => {
                let found = self
                    .settings_view(ui, 0)
                    .rows
                    .into_iter()
                    .find(|r| r.id() == *key);
                let (label, secret, placeholder, said) = match found {
                    Some(Row::Text(t)) => {
                        let placeholder =
                            hints::value_placeholder(true, t.secret, &t.placeholder).to_owned();
                        (t.label, t.secret, placeholder, t.hint)
                    }
                    _ => (
                        "Value".to_owned(),
                        false,
                        hints::value_placeholder(false, false, "").to_owned(),
                        String::new(),
                    ),
                };
                let grown = hints::editing_row(theme, &label, secret, &said);
                (
                    vec![row(grown)],
                    input(theme, draft, &placeholder, secret, width),
                )
            }
            Mode::Picking { at, wish } => {
                let mut grown = vec![row(vec![Span::new("Default model", theme.fg3)])];
                grown.extend(self.default_picker_rows(ui, theme, *at, wish, width));
                (
                    grown,
                    input(theme, &ui.search, hints::SEARCH_PLACEHOLDER, false, width),
                )
            }
            _ => (
                Vec::new(),
                input(theme, &ui.search, hints::SEARCH_PLACEHOLDER, false, width),
            ),
        }
    }

    /// 缺省模型的选择器：选中的模型附近的几个，然后是它的档位。
    fn default_picker_rows(
        &self,
        ui: &Settings,
        theme: &Theme,
        at: usize,
        wish: &str,
        width: usize,
    ) -> Vec<Element> {
        let ms: Vec<CatalogModel> = self.settings_models();
        if ms.is_empty() {
            return vec![row(vec![Span::new(
                "No models are available yet.",
                theme.fg3,
            )])];
        }
        let current = match ui.data.get(&super::data::Section::Models) {
            Some(super::data::SectionData::Models(m)) => m.default_model.clone(),
            _ => None,
        };
        let pw = if width < PICKER_NARROW {
            0
        } else {
            ms.iter()
                .map(|m| string_width(&m.provider_name))
                .max()
                .unwrap_or(0)
                .min(PROVIDER_MAX)
                + 3
        };
        let nw = ms.iter().map(|m| string_width(&m.model)).max().unwrap_or(0) + 3;
        let sel = at.min(ms.len() - 1);
        let (from, n) = picker_window(sel, ms.len());
        let more = |text: String| {
            row(vec![
                Span::new(" ".repeat(pw), Style::DEFAULT),
                Span::new(text, theme.fg3),
            ])
        };
        let chosen = chosen_style(theme);
        let mut out = Vec::new();
        if from > 0 {
            out.push(more(format!("↑ {from} more")));
        }
        for i in from..from + n {
            let m = &ms[i];
            let group = i == from || ms[i - 1].provider != m.provider;
            let mut spans = Vec::new();
            if pw > 0 {
                let name = if group {
                    clip(&m.provider_name, PROVIDER_MAX)
                } else {
                    String::new()
                };
                spans.push(Span::new(pad(&name, pw), theme.fg3));
            }
            spans.push(Span::new(
                m.model.clone(),
                if i == sel { chosen } else { theme.fg2 },
            ));
            let in_use = current
                .as_ref()
                .is_some_and(|c| c.provider == m.provider && c.model == m.model);
            if in_use {
                spans.push(Span::new(
                    " ".repeat(nw.saturating_sub(string_width(&m.model))),
                    Style::DEFAULT,
                ));
                spans.push(Span::new("current", theme.fg3));
            }
            out.push(row(spans));
        }
        if from + n < ms.len() {
            out.push(more(format!("↓ {} more", ms.len() - from - n)));
        }
        let m = &ms[sel];
        let effort = effort_of(m, wish);
        let label = if pw == 0 {
            "Effort ".to_owned()
        } else {
            pad("Effort", pw.saturating_sub(1))
        };
        let mut spans = vec![Span::new(label, theme.fg3)];
        spans.extend(effort_bars(
            theme,
            &m.reasoning_levels,
            m.reasoning_levels.iter().position(|e| *e == effort),
            fade::TICKS,
        ));
        out.push(row(spans));
        out
    }
}

/// 设置页的输入行：箭头，带光标的文字，或占位。值宽过一行时按格折行（留出光标的一格），至多 [`INPUT_MAX_ROWS`] 行，跟着
/// 光标。Puffin 在空格处折行；值只有一行、很少折行，这里按格折。
fn input(theme: &Theme, d: &Field, placeholder: &str, secret: bool, width: usize) -> Element {
    let chevron = vec![
        Span::new(" ", Style::DEFAULT),
        Span::new(PROMPT_ARROW, Style::fg(theme.brand).with_attr(attr::BOLD)),
        Span::new(" ", Style::DEFAULT),
    ];
    if d.text.is_empty() {
        let mut spans = chevron;
        spans.push(Span::new(" ", Style::CARET));
        spans.push(Span::new(placeholder.to_owned(), theme.fg3));
        return single_line(spans, false);
    }
    let shown = if secret {
        // 密钥按字符数显示成圆点；光标在同样多的圆点之后。
        let before = d.text[..d.cursor.min(d.text.len())].chars().count();
        let dot = "•";
        Field::new(dot.repeat(d.text.chars().count()), before * dot.len())
    } else {
        d.clone()
    };
    let w = width.saturating_sub(COMPOSER_INDENT.len()).max(2);
    // 按格折行：每一行的（起，止）字节下标。
    let mut cuts: Vec<(usize, usize)> = Vec::new();
    let (mut start, mut cols) = (0usize, 0usize);
    for (i, ch) in shown.text.char_indices() {
        let cw = string_width(ch.encode_utf8(&mut [0; 4]));
        if cols + cw >= w && i > start {
            cuts.push((start, i));
            start = i;
            cols = 0;
        }
        cols += cw;
    }
    cuts.push((start, shown.text.len()));
    // 光标在行末且这一行满了：它落在下一行的开头。
    let cursor_row = cuts
        .iter()
        .position(|(s, e)| shown.cursor >= *s && shown.cursor < *e)
        .unwrap_or(cuts.len() - 1);
    let text = &shown.text;
    let lines: Vec<Element> = cuts
        .iter()
        .enumerate()
        .map(|(i, &(s, e))| {
            let content = if i != cursor_row {
                vec![Span::new(text[s..e].to_owned(), Style::DEFAULT)]
            } else {
                let next = if shown.cursor < e {
                    shown.right().cursor
                } else {
                    shown.cursor
                };
                let under = if next > shown.cursor {
                    text[shown.cursor..next].to_owned()
                } else {
                    " ".to_owned()
                };
                vec![
                    Span::new(text[s..shown.cursor].to_owned(), Style::DEFAULT),
                    Span::new(under, Style::CARET),
                    Span::new(text[next..e].to_owned(), Style::DEFAULT),
                ]
            };
            let mut spans = if i == 0 {
                chevron.clone()
            } else {
                vec![Span::new(COMPOSER_INDENT, Style::DEFAULT)]
            };
            spans.extend(content.into_iter().filter(|s| !s.text.is_empty()));
            single_line(spans, false)
        })
        .collect();
    clip_rows(stack(lines), INPUT_MAX_ROWS, ClipFocus::Cursor)
}
