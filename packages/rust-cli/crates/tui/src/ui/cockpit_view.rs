//! 驾驶舱的画法（docs/specs/rust-cli/tui-cockpit.md §2）：`page_frame.rs` 算好底部栈之后，这里按正好的高度排一屏：头一行、左边的
//! 列表、右边从第 2 行到底部栈之上的窗格（panel 地；plain 主题没有地）。窄于 110 列或按文件夹时没有窗格。
use std::time::Instant;

use rust_cli_kernel_api::peers::PeerPresence;
use rust_cli_tui_kit::app::widgets::key_hints;
use rust_cli_tui_kit::app::widgets::lists::fade_in;
use rust_cli_tui_kit::app::widgets::playhead::TICK_MS;
use rust_cli_tui_kit::app::widgets::resume_list::snippet;
use rust_cli_tui_kit::render::width::string_width;
use rust_cli_tui_kit::render::{Span, Style, StyledLine};
use rust_cli_tui_kit::theme::theme::Theme;

use super::app::{App, Overlay};
use super::cockpit::{Cockpit, Kind, count_words, kind_of, say, since_of};
use super::i18n::Lang;
use super::lines::truncate;
use super::page_frame::PageBody;
use super::sessions_list::{Found, SessionsList};
use super::sessions_steps::Step;

/// 窄于这个宽度没有窗格（§2.2）。
const WIDE: usize = 110;
pub(super) const NAME_COL: usize = 3;
const AGE_END: usize = 7;
pub(super) const TITLE_COL: usize = 10;
const COUNTS_COL: usize = 20;
/// 换组的行与窗格淡入的 tick（§2.1、§2.2）。
const FADE_TICKS: u128 = 4;

/// 一行：字，属于哪个代理（点击选中它）。
pub(super) struct Row {
    pub(super) spans: Vec<Span>,
    pub(super) owner: Option<usize>,
}

pub(super) fn row(spans: Vec<Span>, owner: Option<usize>) -> Row {
    Row { spans, owner }
}

pub(super) fn width_of(spans: &[Span]) -> usize {
    spans.iter().map(|span| string_width(&span.text)).sum()
}

pub(super) fn pad(spans: &mut Vec<Span>, to: usize, style: Style) {
    let used = width_of(spans);
    if used < to {
        spans.push(Span::new(" ".repeat(to - used), style));
    }
}

/// 紧凑的年龄：`41s`、`14m`、`2h`、`3d`。
pub fn age(ms: u64) -> String {
    let seconds = ms / 1_000;
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3_600 => format!("{}m", seconds / 60),
        3_600..86_400 => format!("{}h", seconds / 3_600),
        _ => format!("{}d", seconds / 86_400),
    }
}

/// 句子里的时长：`14m`、`1h 12m`。
pub(super) fn long_age(ms: u64) -> String {
    let minutes = ms / 60_000;
    if (60..1_440).contains(&minutes) {
        let rest = minutes % 60;
        return if rest == 0 {
            format!("{}h", minutes / 60)
        } else {
            format!("{}h {rest}m", minutes / 60)
        };
    }
    age(ms)
}

/// 按词折行（宽字符按格算）。
pub(super) fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for paragraph in text.lines() {
        let mut line = String::new();
        for ch in paragraph.chars() {
            let mut next = line.clone();
            next.push(ch);
            if string_width(&next) > width.max(1) {
                let cut = line.rfind(' ').filter(|_| ch != ' ');
                let carry = cut.map_or(String::new(), |at| line[at + 1..].to_owned());
                line.truncate(cut.unwrap_or(line.len()));
                out.push(std::mem::take(&mut line));
                line = carry;
            }
            line.push(ch);
        }
        out.push(line);
    }
    out
}

/// 窗格与展开行的事实：fg3 的字，line 色的 `  ·  ` 隔开（设计稿 A）；放不下的截掉。
pub(super) fn facts_line(theme: &Theme, facts: &[String], room: usize) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    for (index, fact) in facts.iter().enumerate() {
        if index > 0 {
            spans.push(Span::new("  ·  ", theme.line));
        }
        spans.push(Span::new(fact.clone(), theme.fg3));
    }
    truncate_spans(&mut spans, room);
    spans
}

pub(super) fn flat(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn mark(theme: &Theme, kind: Kind) -> Span {
    match kind {
        Kind::Need => Span::new("●", Style::fg(theme.code)),
        Kind::Work => Span::new("●", Style::fg(theme.ok)),
        Kind::New => Span::new("○", Style::fg(theme.work)),
        Kind::Failed => Span::new("✗", Style::fg(theme.err)),
        Kind::Rest => Span::new("·", theme.fg3),
    }
}

fn last_segment(path: &str) -> &str {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
}

pub(super) fn presence(x: &Found) -> Option<&PeerPresence> {
    x.presence.as_ref().map(|p| &p.presence)
}

/// 标题的墨（§2.1）。
pub(super) fn title_ink(theme: &Theme, kind: Kind, chosen: bool) -> Style {
    if chosen {
        let style = if theme.rich {
            Style::fg(theme.brand)
        } else {
            Style::BOLD
        };
        return style;
    }
    if kind == Kind::Rest {
        theme.fg2
    } else {
        Style::DEFAULT
    }
}

impl App {
    /// 一页：内容按这一帧的几何排（`fit`，`draw.rs`）。槽是这一页的键（§3.1）。
    pub fn cockpit_page_body(&self) -> Option<PageBody> {
        let Some(Overlay::Sessions(list)) = &self.overlay else {
            return None;
        };
        let cockpit = list.cockpit.as_ref()?;
        if list.reading() {
            return None;
        }
        Some(PageBody {
            fit: true,
            slot: Some(self.cockpit_slot(list, cockpit)),
            ..PageBody::default()
        })
    }

    fn cockpit_slot(&self, list: &SessionsList, cockpit: &Cockpit) -> Vec<Span> {
        let lang = self.lang;
        let key = |k: &str, en: &'static str, zh: &'static str| {
            (k.to_owned(), say(lang, en, zh).to_owned())
        };
        let keys = if list.query.is_empty() {
            vec![
                key("⏎", "open", "打开"),
                key("^B", "new here", "在这里新建"),
                key("^X", "stop", "停止"),
                if cockpit.by_folder {
                    key("⇥", "by need", "按需要")
                } else {
                    key("⇥", "by folder", "按文件夹")
                },
                key("esc", "back", "返回"),
            ]
        } else {
            vec![key("⏎", "open", "打开"), key("esc", "clear", "清除")]
        };
        key_hints::spans(&self.ink.theme, &keys)
    }

    /// 画之前记下这一帧：选中的代理换了、哪一行换了组（淡入）。
    pub(super) fn note_cockpit(list: &mut SessionsList, now: Instant) {
        let chosen = list.chosen_found().map(|x| x.id.clone());
        let kinds: Vec<(String, Kind)> = list
            .all
            .iter()
            .flatten()
            .map(|x| (x.id.clone(), kind_of(x)))
            .collect();
        let Some(cockpit) = list.cockpit.as_mut() else {
            return;
        };
        if cockpit.shown.as_ref().map(|(id, _)| id) != chosen.as_ref() {
            cockpit.shown = chosen.map(|id| (id, now));
        }
        for (id, kind) in kinds {
            match cockpit.kinds.get_mut(&id) {
                Some(entry) if entry.0 != kind => *entry = (kind, now),
                Some(_) => {}
                // 第一次见到的不淡入：打开时整页一起出现。
                None => {
                    let long_ago = now.checked_sub(std::time::Duration::from_secs(60));
                    cockpit.kinds.insert(id, (kind, long_ago.unwrap_or(now)));
                }
            }
        }
    }

    /// 还有东西在淡入（`App::animating`）。
    pub fn cockpit_fading(&self, now: Instant) -> bool {
        let Some(Overlay::Sessions(list)) = &self.overlay else {
            return false;
        };
        let Some(cockpit) = &list.cockpit else {
            return false;
        };
        let moving = |at: &Instant| fade_of(*at, now) < 1.0;
        cockpit.shown.as_ref().is_some_and(|(_, at)| moving(at))
            || cockpit.kinds.values().any(|(_, at)| moving(at))
    }

    /// 一屏（`height` 是底部栈之上的行数）：行与（屏幕行，行的下标）的可点处。
    pub(super) fn cockpit_screen(
        &self,
        width: usize,
        height: usize,
        now: Instant,
        now_ms: u64,
    ) -> (Vec<StyledLine>, Vec<(usize, usize)>) {
        let Some(Overlay::Sessions(list)) = &self.overlay else {
            return (Vec::new(), Vec::new());
        };
        let Some(cockpit) = &list.cockpit else {
            return (Vec::new(), Vec::new());
        };
        let theme = &self.ink.theme;
        let wide = width >= WIDE && !cockpit.by_folder;
        let pane_at = width * 23 / 40;
        let list_end = if wide {
            pane_at - 3
        } else {
            width.saturating_sub(2)
        };
        let shown = list.matches();
        let chosen = list.choice.min(shown.len().saturating_sub(1));
        let rows = if cockpit.by_folder {
            self.folder_rows(list, cockpit, &shown, chosen, list_end, now, now_ms)
        } else {
            self.need_rows(list, &shown, chosen, list_end, wide, now, now_ms)
        };
        // 正文从第 3 行起（§2）。
        let body = height.saturating_sub(3);
        let (window, hits) = self.window(&rows, chosen, body);
        let pane = if wide && list.all.is_some() && !shown.is_empty() {
            self.pane(cockpit, shown[chosen], width - pane_at - 4, now, now_ms)
        } else {
            Vec::new()
        };
        let mut out = Vec::with_capacity(height);
        for index in 0..height {
            let mut spans = match index {
                1 => self.cockpit_head(list, wide),
                0 | 2 => Vec::new(),
                _ => window.get(index - 3).cloned().unwrap_or_default(),
            };
            // 窗格从下一行才开始：头一行可以写到屏宽。
            if index == 1 {
                truncate_spans(&mut spans, width);
            } else if wide {
                truncate_spans(&mut spans, pane_at);
                pad(&mut spans, pane_at, Style::DEFAULT);
                // 窗格的地从第 2 行起（下标 2）；plain 没有地，读的那一列不动。
                if index >= 2 {
                    let ground = |style: Style| match theme.panel {
                        Some(color) => style.with_bg(color),
                        None => style,
                    };
                    let mut right = vec![Span::new("  ", ground(Style::DEFAULT))];
                    // 第 2 行只有地，字从第 3 行起，与左边的第一行对齐（设计稿 A）。
                    let text = index
                        .checked_sub(3)
                        .and_then(|at| pane.get(at))
                        .cloned()
                        .unwrap_or_default();
                    right.extend(text.into_iter().map(|span| {
                        let style = ground(span.style);
                        Span::new(span.text, style)
                    }));
                    pad(&mut right, width - pane_at, ground(Style::DEFAULT));
                    spans.extend(right);
                }
            }
            out.push(StyledLine::new(spans));
        }
        let hits = hits.into_iter().map(|(at, i)| (at + 3, i)).collect();
        (out, hits)
    }

    /// 头一行：名字与数量；筛选时 `4 of 40 match`（§1.1、§2.3）。窄时只写数字。
    fn cockpit_head(&self, list: &SessionsList, wide: bool) -> Vec<Span> {
        let theme = &self.ink.theme;
        let lang = self.lang;
        let name = say(lang, "Agent cockpit", "代理驾驶舱");
        let mut spans = vec![
            Span::plain(" ".repeat(NAME_COL)),
            Span::new(name, Style::BOLD),
        ];
        let at = COUNTS_COL.max(NAME_COL + string_width(name) + 4);
        pad(&mut spans, at, Style::DEFAULT);
        let Some(all) = &list.all else {
            return spans;
        };
        if !list.query.is_empty() {
            let (n, total) = (list.matches().len(), all.len());
            let text = if lang == Lang::Zh {
                format!("{total} 个里有 {n} 个符合")
            } else {
                format!("{n} of {total} match")
            };
            spans.push(Span::new(text, theme.fg3));
            return spans;
        }
        let count = |kind: Kind| all.iter().filter(|x| kind_of(x) == kind).count();
        for kind in [Kind::Need, Kind::Work, Kind::New, Kind::Failed, Kind::Rest] {
            let n = count(kind);
            if n == 0 {
                continue;
            }
            spans.push(mark(theme, kind));
            let text = if wide {
                count_words(lang, kind, n)
            } else {
                n.to_string()
            };
            spans.push(Span::new(format!(" {text}   "), theme.fg3));
        }
        spans
    }

    /// 窗口跟着选中的行；放不下时写 `↑ N more` / `↓ N more`（N 数代理，§2.1）。
    fn window(
        &self,
        rows: &[Row],
        chosen: usize,
        body: usize,
    ) -> (Vec<Vec<Span>>, Vec<(usize, usize)>) {
        let theme = &self.ink.theme;
        if body == 0 {
            return (Vec::new(), Vec::new());
        }
        let last = rows
            .iter()
            .rposition(|row| row.owner == Some(chosen))
            .unwrap_or(0);
        let over = rows.len() > body;
        // 选中的行之下留一行写 `↓`。
        let top = if over && last + 2 > body {
            (last + 2 - body).min(rows.len() - body)
        } else {
            0
        };
        let end = (top + body).min(rows.len());
        let agents = |slice: &[Row]| {
            let mut owners: Vec<usize> = slice.iter().filter_map(|row| row.owner).collect();
            owners.dedup();
            owners.len()
        };
        let more = |text: String| {
            vec![
                Span::plain(" ".repeat(NAME_COL)),
                Span::new(text, theme.fg3),
            ]
        };
        let mut lines = Vec::new();
        let mut hits = Vec::new();
        for (at, row) in rows[top..end].iter().enumerate() {
            lines.push(row.spans.clone());
            if let Some(owner) = row.owner {
                hits.push((at, owner));
            }
        }
        if top > 0 {
            lines[0] = more(format!("↑ {} more", agents(&rows[..=top])));
            hits.retain(|(at, _)| *at != 0);
        }
        if end < rows.len() {
            let n = lines.len() - 1;
            lines[n] = more(format!("↓ {} more", agents(&rows[end - 1..])));
            hits.retain(|(at, _)| *at != n);
        }
        (lines, hits)
    }

    pub(super) fn agent_fade(&self, list: &SessionsList, id: &str, now: Instant) -> f64 {
        list.cockpit
            .as_ref()
            .and_then(|cockpit| cockpit.kinds.get(id))
            .map_or(1.0, |(_, at)| fade_of(*at, now))
    }

    /// 按需要分组（§2.1）。
    #[allow(clippy::too_many_arguments)]
    fn need_rows(
        &self,
        list: &SessionsList,
        shown: &[&Found],
        chosen: usize,
        end: usize,
        wide: bool,
        now: Instant,
        now_ms: u64,
    ) -> Vec<Row> {
        let theme = &self.ink.theme;
        let lang = self.lang;
        let mut rows = Vec::new();
        if shown.is_empty() {
            if list.all.is_some() {
                rows.push(row(self.empty_row(list), None));
            }
            return rows;
        }
        let names = [
            say(lang, "Needs you", "等你"),
            say(lang, "Working", "在跑"),
            say(lang, "Finished", "做完了"),
            say(lang, "Resting", "歇着"),
        ];
        let mut group = None;
        for (index, x) in shown.iter().enumerate() {
            let kind = kind_of(x);
            if group != Some(kind.group()) {
                if group.is_some() {
                    rows.push(row(Vec::new(), None));
                }
                group = Some(kind.group());
                rows.push(row(
                    vec![
                        Span::plain(" ".repeat(NAME_COL)),
                        Span::new(names[kind.group()], theme.fg3),
                    ],
                    None,
                ));
            }
            let fade = self.agent_fade(list, &x.id, now);
            let faded = |mut spans: Vec<Span>| {
                for span in &mut spans {
                    span.style = fade_in(theme, span.style, fade);
                }
                spans
            };
            rows.push(row(
                faded(self.agent_title_row(x, kind, index == chosen, end, now_ms)),
                Some(index),
            ));
            if let Some(second) = self.second_row(list, x, kind, end) {
                rows.push(row(faded(second), Some(index)));
            }
            // 窄时没有窗格：选中的行下面原地多两行（§2.2）。
            if !wide && index == chosen {
                for line in self.inline_rows(x, kind, end, now_ms) {
                    rows.push(row(line, Some(index)));
                }
            }
        }
        rows
    }

    pub(super) fn empty_row(&self, list: &SessionsList) -> Vec<Span> {
        let text = if list.query.is_empty() {
            say(self.lang, "No agents yet.", "还没有代理。").to_owned()
        } else if self.lang == Lang::Zh {
            format!("没有符合“{}”的代理。", list.query)
        } else {
            format!("No agent matches “{}”.", list.query)
        };
        vec![
            Span::plain(" ".repeat(NAME_COL)),
            Span::new(text, self.ink.theme.fg3),
        ]
    }

    /// 年龄、记号、标题、`current`、右端的文件夹。
    fn agent_title_row(
        &self,
        x: &Found,
        kind: Kind,
        chosen: bool,
        end: usize,
        now_ms: u64,
    ) -> Vec<Span> {
        let theme = &self.ink.theme;
        let mut spans = self.lead(x, kind, now_ms);
        let mut right = Vec::new();
        if let Some(machine) = presence(x).and_then(|p| p.machine.as_deref()) {
            right.push(Span::new(format!("@{machine} "), Style::fg(theme.agent)));
        }
        right.push(Span::new(last_segment(&x.root).to_owned(), theme.fg3));
        let current = self.session.as_deref() == Some(x.id.as_str());
        let tail: Vec<Span> = if current {
            vec![
                Span::new("  ·  ", theme.line),
                Span::new(say(self.lang, "current", "现在"), theme.fg3),
            ]
        } else {
            Vec::new()
        };
        let room = end
            .saturating_sub(TITLE_COL + width_of(&right) + width_of(&tail) + 2)
            .max(4);
        spans.push(Span::new(
            truncate(&x.title, room),
            title_ink(theme, kind, chosen),
        ));
        spans.extend(tail);
        pad(
            &mut spans,
            end.saturating_sub(width_of(&right)),
            Style::DEFAULT,
        );
        spans.extend(right);
        spans
    }

    pub(super) fn lead(&self, x: &Found, kind: Kind, now_ms: u64) -> Vec<Span> {
        let theme = &self.ink.theme;
        let text = age(now_ms.saturating_sub(since_of(x)));
        vec![
            Span::plain(" ".repeat(AGE_END.saturating_sub(string_width(&text)))),
            Span::new(text, theme.fg3),
            Span::plain(" "),
            mark(theme, kind),
            Span::plain(" "),
        ]
    }

    /// 多的一行（§2.1）：问题或正在做的一行；筛选命中别处时是命中的那段（§2.3）。
    fn second_row(
        &self,
        list: &SessionsList,
        x: &Found,
        kind: Kind,
        end: usize,
    ) -> Option<Vec<Span>> {
        let theme = &self.ink.theme;
        let room = end.saturating_sub(TITLE_COL);
        let lead = Span::plain(" ".repeat(TITLE_COL));
        if !list.query.is_empty() && !x.title.to_lowercase().contains(&list.query.to_lowercase()) {
            let hit = [&x.prompt, &x.reply]
                .iter()
                .find_map(|text| snippet(text, &list.query.to_lowercase()))?;
            return Some(vec![lead, Span::new(truncate(&hit, room), theme.fg3)]);
        }
        let p = presence(x);
        let (text, ink) = match kind {
            Kind::Need => {
                let ask = p.and_then(|p| p.ask.as_ref())?;
                let text = match &ask.detail {
                    Some(detail) => format!("{}  {}", flat(&ask.question), flat(detail)),
                    None => flat(&ask.question),
                };
                (text, theme.fg2)
            }
            Kind::Work => (
                flat(&p.and_then(|p| p.now.clone()).unwrap_or_default()),
                theme.fg3,
            ),
            _ => return None,
        };
        let agents = p.map_or(0, |p| p.agents.iter().filter(|a| a.running).count());
        let more = (agents > 0).then(|| match self.lang {
            Lang::En => format!("  +{agents} subagents"),
            Lang::Zh => format!("  +{agents} 个子代理"),
        });
        let suffix = more.as_deref().map_or(0, string_width);
        if text.is_empty() && more.is_none() {
            return None;
        }
        let mut spans = vec![
            lead,
            Span::new(truncate(&text, room.saturating_sub(suffix)), ink),
        ];
        spans.extend(more.map(|more| Span::new(more, theme.fg3)));
        Some(spans)
    }

    /// 窄时选中的行下面的两行（问题；`分支 · 上下文 · 在哪里`）。
    fn inline_rows(&self, x: &Found, kind: Kind, end: usize, now_ms: u64) -> Vec<Vec<Span>> {
        let theme = &self.ink.theme;
        let room = end.saturating_sub(TITLE_COL);
        let lead = || Span::plain(" ".repeat(TITLE_COL));
        let mut out = Vec::new();
        let (say_it, ink) = self.state_words(x, kind, now_ms);
        out.push(vec![lead(), Span::new(truncate(&say_it, room), ink)]);
        let mut facts: Vec<String> = Vec::new();
        facts.extend(x.branch.clone());
        facts.extend(self.context_of(x).map(|c| format!("{c}%")));
        facts.extend(self.place_of(x));
        let mut line = vec![lead()];
        line.extend(facts_line(theme, &facts, room));
        out.push(line);
        out
    }

    /// 上下文：活着的从现场读，其余从读回的会话。
    pub(super) fn context_of(&self, x: &Found) -> Option<u32> {
        if let Some(context) = presence(x).and_then(|p| p.context) {
            return Some(context);
        }
        let Some(Overlay::Sessions(list)) = &self.overlay else {
            return None;
        };
        let peek = list.cockpit.as_ref()?.peek.as_ref()?;
        (peek.id == x.id).then_some(peek.context).flatten()
    }

    pub(super) fn place_of(&self, x: &Found) -> Option<String> {
        let presence = x.presence.as_ref()?;
        if let Some(machine) = &presence.presence.machine {
            return Some(match self.lang {
                Lang::En => format!("on {machine}"),
                Lang::Zh => format!("在 {machine} 上"),
            });
        }
        Some(if presence.here {
            say(self.lang, "in this terminal", "在这个终端").to_owned()
        } else {
            say(self.lang, "in another terminal", "在别的终端").to_owned()
        })
    }

    /// `一句话` 与它的墨（§2.2）。
    pub(super) fn state_words(&self, x: &Found, kind: Kind, now_ms: u64) -> (String, Style) {
        let theme = &self.ink.theme;
        let ago = long_age(now_ms.saturating_sub(since_of(x)));
        let zh = self.lang == Lang::Zh;
        let text = match kind {
            Kind::Need if zh => format!("在问你，{ago}了"),
            Kind::Need => format!("Asking you, for {ago}"),
            Kind::Work if zh => format!("在跑，{ago}了"),
            Kind::Work => format!("Working, for {ago}"),
            Kind::New if zh => format!("{ago}前做完。你还没读"),
            Kind::New => format!("Finished {ago} ago. You have not read it yet"),
            Kind::Failed if zh => format!("{ago}前停下"),
            Kind::Failed => format!("Stopped {ago} ago"),
            Kind::Rest if zh => format!("{ago}前用过"),
            Kind::Rest => format!("Last active {ago} ago"),
        };
        (text, theme.fg2)
    }

    pub(super) fn kind_word(&self, kind: Kind) -> &'static str {
        let lang = self.lang;
        match kind {
            Kind::Need => say(lang, "needs you", "等你"),
            Kind::Work => say(lang, "working", "在跑"),
            Kind::New => say(lang, "finished", "做完了"),
            Kind::Failed => say(lang, "failed", "失败"),
            Kind::Rest => say(lang, "resting", "歇着"),
        }
    }

    /// `?`：板上一层，这一页的键；任何键关上（§3.1）。
    pub(super) fn cockpit_keys_rows(&self) -> Vec<rust_cli_tui_kit::app::element::Element> {
        let theme = &self.ink.theme;
        let lang = self.lang;
        let entries = [
            ("← esc", say(lang, "back", "返回")),
            ("⏎ →", say(lang, "open", "打开")),
            ("↑↓ ^P ^N", say(lang, "choose", "选择")),
            ("⇥", say(lang, "by need or by folder", "按需要或按文件夹")),
            (
                "^B",
                say(lang, "new agent in its folder", "在它的文件夹新建"),
            ),
            ("^X", say(lang, "stop it", "停止它")),
            ("^F", say(lang, "filter by state", "按状态筛")),
            ("^E", say(lang, "created order", "按创建时间排")),
            ("^R", say(lang, "rename", "改名")),
            ("^D", say(lang, "delete", "删除")),
            ("^T", say(lang, "read it", "读对话")),
        ];
        entries
            .iter()
            .map(|(key, what)| {
                let mut spans = vec![
                    Span::plain(" ".repeat(NAME_COL)),
                    Span::new(*key, Style::fg(theme.brand)),
                ];
                pad(&mut spans, NAME_COL + 12, Style::DEFAULT);
                spans.push(Span::new(*what, theme.fg2));
                rust_cli_tui_kit::app::element::Element::Raw(vec![StyledLine::new(spans)])
            })
            .collect()
    }

    /// 这一行在窗格之外是不是还在读（`Step::Keys` 也算板上）。
    pub(super) fn cockpit_step_on_board(step: &Step) -> bool {
        !matches!(step, Step::List | Step::Reading { .. } | Step::Read(_))
    }
}

/// 从 `at` 起淡入的进度（0 到 1）。
pub(super) fn fade_of(at: Instant, now: Instant) -> f64 {
    let ticks = now.saturating_duration_since(at).as_millis() / u128::from(TICK_MS);
    if ticks >= FADE_TICKS {
        1.0
    } else {
        (ticks + 1) as f64 / (FADE_TICKS + 1) as f64
    }
}

/// 截到 `width` 格（左边的列表不伸进窗格）。
fn truncate_spans(spans: &mut Vec<Span>, width: usize) {
    let mut used = 0;
    let mut keep = Vec::with_capacity(spans.len());
    for span in spans.drain(..) {
        let w = string_width(&span.text);
        if used + w <= width {
            used += w;
            keep.push(span);
        } else {
            let room = width - used;
            let style = span.style;
            if room > 0 {
                keep.push(Span::new(truncate(&span.text, room), style));
            }
            break;
        }
    }
    *spans = keep;
}
