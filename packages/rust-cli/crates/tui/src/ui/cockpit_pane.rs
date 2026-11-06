//! 驾驶舱的窗格与按文件夹的行（docs/specs/rust-cli/tui-cockpit.md §2.1、§2.2）；一屏的排法在 `cockpit_view.rs`。
use std::time::Instant;

use rust_cli_tui_kit::app::widgets::lists::fade_in;
use rust_cli_tui_kit::render::width::string_width;
use rust_cli_tui_kit::render::{Span, Style};

use super::app::App;
use super::cockpit::say;
use super::cockpit::{Cockpit, Kind, kind_of};
use super::cockpit_view::{
    NAME_COL, Row, TITLE_COL, age, facts_line, fade_of, flat, long_age, mark, pad, presence, row,
    title_ink, width_of, wrap,
};
use super::i18n::Lang;
use super::lines::truncate;
use super::sessions_list::{Found, SessionsList};

/// 按文件夹时的两列（§2.1）。
const STATE_COL: usize = 50;
const NOW_COL: usize = 61;

impl App {
    /// 窗格（§2.2）：依次，没有的不写。`room` 是字的宽度；每行都在切换时一起淡入。
    pub(super) fn pane(
        &self,
        cockpit: &Cockpit,
        x: &Found,
        room: usize,
        now: Instant,
        now_ms: u64,
    ) -> Vec<Vec<Span>> {
        let theme = &self.ink.theme;
        let kind = kind_of(x);
        let peek = cockpit.peek.as_ref().filter(|peek| peek.id == x.id);
        let p = presence(x);
        let mut out: Vec<Vec<Span>> = Vec::new();
        let line = |text: String, style: Style| vec![Span::new(truncate(&text, room), style)];
        out.push(line(x.title.clone(), Style::BOLD));
        out.push(line(x.folder.clone(), theme.fg3));
        let mut facts: Vec<String> = Vec::new();
        facts.extend(x.branch.clone());
        facts.extend(
            peek.map(|peek| peek.model.clone())
                .filter(|m| !m.is_empty()),
        );
        facts.extend(self.context_of(x).map(|c| format!("{c}%")));
        if !facts.is_empty() {
            out.push(facts_line(theme, &facts, room));
        }
        let mut place: Vec<String> = Vec::new();
        place.extend(self.place_of(x));
        place.push(match self.lang {
            Lang::En => format!("started {} ago", long_age(now_ms.saturating_sub(x.created))),
            Lang::Zh => format!("{}前开始", long_age(now_ms.saturating_sub(x.created))),
        });
        out.push(facts_line(theme, &place, room));
        out.push(Vec::new());
        let (words, ink) = self.state_words(x, kind, now_ms);
        let mut head = vec![mark(theme, kind), Span::plain(" ")];
        head.push(Span::new(truncate(&words, room.saturating_sub(2)), ink));
        out.push(head);
        out.push(Vec::new());
        match kind {
            Kind::Need => {
                if let Some(ask) = p.and_then(|p| p.ask.as_ref()) {
                    for text in wrap(&ask.question, room) {
                        out.push(vec![Span::new(text, Style::DEFAULT)]);
                    }
                    if let Some(detail) = &ask.detail {
                        for text in wrap(detail, room) {
                            out.push(vec![Span::new(text, Style::fg(theme.code))]);
                        }
                    }
                    out.push(Vec::new());
                    // 没有编号：按不了（§2.2）。
                    let widest = ask
                        .answers
                        .iter()
                        .map(|a| string_width(&a.label))
                        .max()
                        .unwrap_or(0);
                    for answer in &ask.answers {
                        let mut spans = vec![Span::new(answer.label.clone(), theme.fg2)];
                        if !answer.note.is_empty() {
                            pad(&mut spans, widest + 3, Style::DEFAULT);
                            let room = room.saturating_sub(widest + 3);
                            spans.push(Span::new(truncate(&answer.note, room), theme.fg3));
                        }
                        out.push(spans);
                    }
                    out.push(Vec::new());
                }
            }
            Kind::Failed => {
                for text in wrap(x.note.failed.as_deref().unwrap_or_default(), room) {
                    out.push(vec![Span::new(text, Style::fg(theme.err))]);
                }
                out.push(Vec::new());
            }
            Kind::Work => {
                for (tool, text) in peek.map(|peek| peek.tail.as_slice()).unwrap_or_default() {
                    let ink = if *tool { theme.fg2 } else { theme.fg3 };
                    out.push(line(text.clone(), ink));
                }
                if peek.is_some_and(|peek| !peek.tail.is_empty()) {
                    out.push(Vec::new());
                }
            }
            _ => {}
        }
        if let Some(p) = p.filter(|p| !p.agents.is_empty() || p.todos.is_some()) {
            let mut head = say(self.lang, "Subagents", "子代理").to_owned();
            if let Some((done, total)) = p.todos {
                head = match self.lang {
                    Lang::En => format!("{head}  ·  {done} of {total} todos done"),
                    Lang::Zh => format!("{head}  ·  待办做完 {done} / {total}"),
                };
            }
            out.push(line(head, theme.fg3));
            for agent in &p.agents {
                let elapsed = if agent.running {
                    agent.elapsed_ms + now_ms.saturating_sub(agent.at_ms)
                } else {
                    agent.elapsed_ms
                };
                let elapsed = age(elapsed);
                let mut spans = vec![if agent.running {
                    Span::new("● ", Style::fg(theme.ok))
                } else {
                    Span::new("✓ ", Style::fg(theme.ok))
                }];
                spans.push(Span::new(agent.name.clone(), theme.fg2));
                spans.push(Span::plain("  "));
                let room_left = room.saturating_sub(width_of(&spans) + string_width(&elapsed) + 1);
                spans.push(Span::new(
                    truncate(&flat(&agent.doing), room_left),
                    theme.fg3,
                ));
                pad(
                    &mut spans,
                    room.saturating_sub(string_width(&elapsed)),
                    Style::DEFAULT,
                );
                spans.push(Span::new(elapsed, theme.fg3));
                out.push(spans);
            }
            out.push(Vec::new());
        }
        let first = flat(&x.prompt);
        if !first.is_empty() {
            out.push(line(format!("› {first}"), theme.fg3));
        }
        let reply = peek
            .map(|peek| peek.reply.as_str())
            .filter(|r| !r.is_empty())
            .unwrap_or(&x.reply);
        for text in wrap(&flat(reply), room) {
            out.push(vec![Span::new(text, theme.fg2)]);
        }
        let fade = cockpit
            .shown
            .as_ref()
            .map_or(1.0, |(_, at)| fade_of(*at, now));
        for spans in &mut out {
            for span in spans.iter_mut() {
                span.style = fade_in(theme, span.style, fade);
            }
        }
        out
    }

    /// 按文件夹（§2.1）：组头、多两列的行、子代理在下面、选中的行原地展开。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn folder_rows(
        &self,
        list: &SessionsList,
        cockpit: &Cockpit,
        shown: &[&Found],
        chosen: usize,
        end: usize,
        now: Instant,
        now_ms: u64,
    ) -> Vec<Row> {
        let theme = &self.ink.theme;
        let mut rows = Vec::new();
        if shown.is_empty() {
            if list.all.is_some() {
                rows.push(row(self.empty_row(list), None));
            }
            return rows;
        }
        let mut root: Option<&str> = None;
        for (index, x) in shown.iter().enumerate() {
            if root != Some(x.root.as_str()) {
                if root.is_some() {
                    rows.push(row(Vec::new(), None));
                }
                root = Some(x.root.as_str());
                let mut head = vec![Span::plain(" ".repeat(NAME_COL))];
                if let Some(machine) = presence(x).and_then(|p| p.machine.as_deref()) {
                    head.push(Span::new(format!("@{machine}  "), Style::fg(theme.agent)));
                }
                head.push(Span::new(x.folder.clone(), theme.fg2));
                if let Some(branch) = &x.branch {
                    head.push(Span::new("  ·  ", theme.line));
                    head.push(Span::new(branch.clone(), theme.fg3));
                }
                rows.push(row(head, None));
            }
            let kind = kind_of(x);
            let mut spans = self.lead(x, kind, now_ms);
            let has_columns = end > NOW_COL + 8;
            let title_room = if has_columns {
                STATE_COL - TITLE_COL - 2
            } else {
                end.saturating_sub(TITLE_COL + 1)
            };
            spans.push(Span::new(
                truncate(&x.title, title_room),
                title_ink(theme, kind, index == chosen),
            ));
            let context = self.context_of(x).map(|c| format!("{c}%"));
            if has_columns {
                pad(&mut spans, STATE_COL, Style::DEFAULT);
                let word = self.kind_word(kind);
                spans.push(Span::new(word, theme.fg3));
                pad(&mut spans, NOW_COL, Style::DEFAULT);
                let p = presence(x);
                let text = match kind {
                    Kind::Need => p.and_then(|p| p.ask.as_ref()).map(|a| a.question.clone()),
                    Kind::Work => p.and_then(|p| p.now.clone()),
                    _ => None,
                }
                .unwrap_or_default();
                let room = end.saturating_sub(NOW_COL + 6);
                spans.push(Span::new(truncate(&flat(&text), room), theme.fg3));
                if let Some(context) = &context {
                    pad(
                        &mut spans,
                        end.saturating_sub(string_width(context)),
                        Style::DEFAULT,
                    );
                    spans.push(Span::new(context.clone(), theme.fg3));
                }
            }
            let fade = self.agent_fade(list, &x.id, now);
            for span in &mut spans {
                span.style = fade_in(theme, span.style, fade);
            }
            rows.push(row(spans, Some(index)));
            let lead = || Span::plain(" ".repeat(TITLE_COL));
            for agent in presence(x).map(|p| p.agents.as_slice()).unwrap_or_default() {
                rows.push(row(
                    vec![
                        lead(),
                        Span::new(if agent.running { "● " } else { "✓ " }, Style::fg(theme.ok)),
                        Span::new(agent.name.clone(), theme.fg2),
                        Span::plain("  "),
                        Span::new(
                            truncate(&flat(&agent.doing), end.saturating_sub(TITLE_COL + 20)),
                            theme.fg3,
                        ),
                    ],
                    Some(index),
                ));
            }
            if index == chosen {
                let room = end.saturating_sub(TITLE_COL);
                let peek = cockpit.peek.as_ref().filter(|peek| peek.id == x.id);
                let mut facts: Vec<String> = Vec::new();
                facts.extend(
                    peek.map(|peek| peek.model.clone())
                        .filter(|m| !m.is_empty()),
                );
                facts.extend(self.place_of(x));
                facts.push(match self.lang {
                    Lang::En => {
                        format!("started {} ago", long_age(now_ms.saturating_sub(x.created)))
                    }
                    Lang::Zh => format!("{}前开始", long_age(now_ms.saturating_sub(x.created))),
                });
                let mut line = vec![lead()];
                line.extend(facts_line(theme, &facts, room));
                rows.push(row(line, Some(index)));
                let first = flat(&x.prompt);
                if !first.is_empty() {
                    rows.push(row(
                        vec![
                            lead(),
                            Span::new(truncate(&format!("› {first}"), room), theme.fg3),
                        ],
                        Some(index),
                    ));
                }
                let reply = peek
                    .map(|peek| peek.reply.as_str())
                    .filter(|r| !r.is_empty())
                    .unwrap_or(&x.reply);
                let reply = flat(reply);
                if !reply.is_empty() {
                    rows.push(row(
                        vec![lead(), Span::new(truncate(&reply, room), theme.fg2)],
                        Some(index),
                    ));
                }
            }
        }
        rows
    }
}
