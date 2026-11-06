//! 标题行（docs/specs/rust-cli/tui-tasks.md §2）：输入框正上方一行，左边是计数与 `↓/^T all tasks`，右边是会话名。以前这里是后台
//! 子代理的坞与待办的坞（各自几行），状态栏另有命令的 pill（用户决定 2026-10-31，合成一行）。数只从内核的快照
//! （`App.agents`、`App.background`）、视图的清单与坞记下的「结束了、结果还没到」算，任务卡的种类一行用同一份
//! （[`TaskCounts`]），所以一行与卡永远一致。
use std::collections::HashMap;
use std::time::Instant;

use rust_cli_kernel_api::ids::AgentId;
use rust_cli_kernel_api::manage::BackgroundKind;
use rust_cli_kernel_api::panels::{SubagentEntry, SubagentStatus};
use rust_cli_tui_kit::app::widgets::fade::{self, TICKS};
use rust_cli_tui_kit::app::widgets::key_hints::edge;
use rust_cli_tui_kit::app::widgets::pen::caret_style;
use rust_cli_tui_kit::render::width::string_width;
use rust_cli_tui_kit::render::{Span, Style, StyledLine};
use rust_cli_tui_kit::theme::theme::Theme;

use super::agents_tree::Forest;
use super::app::App;
use super::i18n::Lang;
use super::interaction::ticks_since;
use super::mouse::Target;
use super::task_card::TaskKind;

/// 呼吸按这个长度数 tick（kit `playhead::TICK_MS`，同重试行与以前的坞）。
const TICK_MS: u64 = 33;

/// 标题行左右两边至少隔这么多列（tui-tasks.md §2.3）。
const NAME_GAP: usize = 3;
/// 名字至少要这么多列，不然不写。
const NAME_MIN: usize = 10;
/// 计数只为名字短于这么多列的部分缩短；再长的名字先截自己（tui-tasks.md §2.2「窄的时候」）。
const NAME_WANTS: usize = 16;
/// 子代理的描述剩下不足这么多列时计数走开。
const LABEL_MIN: usize = 12;

/// 标题行右边写的字（`input_box::title`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Title {
    pub text: String,
    pub style: Style,
    /// 不走开（子代理的描述）：放不下时计数先走。
    pub stays: bool,
}

/// 一行与种类一行共用的数（tui-tasks.md §2.1）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TaskCounts {
    /// 等你回答的子代理。
    pub waiting: usize,
    /// 在跑（含停放）、不在等你的后台子代理（沿 owner 走到的顶层是后台的）。
    pub running: usize,
    /// 结束了、结果还没到主会话的顶层后台子代理，按终态。
    pub finished: usize,
    pub errored: usize,
    pub stopped: usize,
    /// 在跑的命令：shell 与 monitor 分开数（修复线 F117，写法是 `manage::command_text`）。
    pub shells: usize,
    pub monitors: usize,
    /// 待办的完成数与总数；以前的待办坞此刻不显示时没有。
    pub todos: Option<(usize, usize)>,
}

/// 一行的记号是哪一种（换了时新记号从 line 色渐变到它的颜色）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineMark {
    Waiting,
    Running,
    Errored,
    Finished,
    Stopped,
    Plan,
}

/// 这一行出现与记号换了的时刻（动画时钟，`draw::draw` 画之前记下）。
#[derive(Debug, Clone, Default)]
pub struct LineSeen {
    at: Option<Instant>,
    mark: Option<LineMark>,
    mark_at: Option<Instant>,
}

impl TaskCounts {
    /// 在跑的命令（shell 与 monitor 合起来；任务卡的 Commands 种类按它数）。
    pub fn commands(&self) -> usize {
        self.shells + self.monitors
    }

    /// 最急的一种（tui-tasks.md §2.2「记号」）。
    pub fn mark(&self) -> Option<LineMark> {
        Some(if self.waiting > 0 {
            LineMark::Waiting
        } else if self.running > 0 || self.commands() > 0 {
            LineMark::Running
        } else if self.errored > 0 {
            LineMark::Errored
        } else if self.finished > 0 {
            LineMark::Finished
        } else if self.stopped > 0 {
            LineMark::Stopped
        } else if self.todos.is_some() {
            LineMark::Plan
        } else {
            return None;
        })
    }
}

/// 沿 `owner` 走到顶层，顶层的是后台子代理：主会话的前台子代理与它派生的不算（transcript 的 Agent 行在说它们）。
fn in_background(entry: &SubagentEntry, by_id: &HashMap<&AgentId, &SubagentEntry>) -> bool {
    let mut at = entry;
    for _ in 0..by_id.len() {
        match at.owner.as_ref().and_then(|owner| by_id.get(owner)) {
            Some(owner) => at = owner,
            None => break,
        }
    }
    at.background
}

/// 标题行：排好的一行与左边可点的地方（起始列，宽度，目标；tui-tasks.md §2.2「点击」）。
#[derive(Debug, Clone, PartialEq)]
pub struct TitleRow {
    pub line: StyledLine,
    pub clicks: Vec<(usize, usize, Target)>,
}

/// 一段字与点它去哪里（`None` 是不能点的空格与 ` · `）。
type Tagged = (Span, Option<Target>);

/// 一个部分：全写与短写（tui-tasks.md §2.2 的表），点它时卡落在的种类。
struct Part {
    full: Vec<Span>,
    short: Vec<Span>,
    kind: TaskKind,
}

/// 数与名词 fg2、动词 fg3（`verb` 连着它前面的空格；中文没有空格）。
fn plain_part(theme: &Theme, count: String, verb: &str, short: String, kind: TaskKind) -> Part {
    Part {
        full: vec![
            Span::new(count, theme.fg2),
            Span::new(verb.to_owned(), theme.fg3),
        ],
        short: vec![Span::new(short, theme.fg2)],
        kind,
    }
}

/// 整段一种墨（等你回答、完成、出错、停了：都是子代理）。
fn inked_part(style: Style, full: String, short: String) -> Part {
    Part {
        full: vec![Span::new(full, style)],
        short: vec![Span::new(short, style)],
        kind: TaskKind::Subagents,
    }
}

fn parts(theme: &Theme, counts: &TaskCounts, lang: Lang) -> Vec<Part> {
    let en = lang == Lang::En;
    let noun = |n: usize, one: &str, many: &str| {
        if n == 1 {
            format!("1 {one}")
        } else {
            format!("{n} {many}")
        }
    };
    let mut out = Vec::new();
    let n = counts.waiting;
    if n > 0 {
        let full = if en {
            format!("{} waiting for you", noun(n, "subagent", "subagents"))
        } else {
            format!("{n} 个子代理等你回答")
        };
        let short = if en {
            format!("{n} waiting")
        } else {
            format!("{n} 个等你")
        };
        out.push(inked_part(Style::fg(theme.code), full, short));
    }
    let n = counts.running;
    if n > 0 {
        out.push(if en {
            let subagents = noun(n, "subagent", "subagents");
            plain_part(
                theme,
                subagents.clone(),
                " running",
                subagents,
                TaskKind::Subagents,
            )
        } else {
            plain_part(
                theme,
                format!("{n} 个子代理"),
                "在跑",
                format!("{n} 个子代理"),
                TaskKind::Subagents,
            )
        });
    }
    for (n, style, en_word, zh_word, zh_short) in [
        (
            counts.finished,
            Style::fg(theme.ok),
            "finished",
            "完成了",
            "完成",
        ),
        (
            counts.errored,
            Style::fg(theme.err),
            "errored",
            "出错了",
            "出错",
        ),
        (counts.stopped, theme.fg3, "stopped", "停了", "停了"),
    ] {
        if n > 0 {
            let (full, short) = if en {
                (
                    format!("{} {en_word}", noun(n, "subagent", "subagents")),
                    format!("{n} {en_word}"),
                )
            } else {
                (
                    format!("{n} 个子代理{zh_word}"),
                    format!("{n} 个{zh_short}"),
                )
            };
            out.push(inked_part(style, full, short));
        }
    }
    // 命令一段的数与名词和回合结束那一行的 `still running` 同一个写法（`1 shell, 2 monitors`；中文 `1 shell、2 monitor`，
    // 名词不译、不带“个”，用户决定 2026-10-30，修复线 F118）。中文的名词是英文，动词前空一格。
    if let Some(commands) = super::manage::command_text(counts.shells, counts.monitors, lang) {
        let verb = if en { " running" } else { " 在跑" };
        out.push(plain_part(
            theme,
            commands.clone(),
            verb,
            commands,
            TaskKind::Commands,
        ));
    }
    if let Some((done, total)) = counts.todos {
        out.push(if en {
            plain_part(
                theme,
                format!("{done} of {total} todos"),
                " done",
                format!("{done}/{total} todos"),
                TaskKind::Todos,
            )
        } else {
            Part {
                full: vec![
                    Span::new("待办完成".to_owned(), theme.fg3),
                    Span::new(format!(" {done}/{total}"), theme.fg2),
                ],
                short: vec![
                    Span::new("待办".to_owned(), theme.fg3),
                    Span::new(format!(" {done}/{total}"), theme.fg2),
                ],
                kind: TaskKind::Todos,
            }
        });
    }
    out
}

fn width_of(spans: &[Span]) -> usize {
    spans.iter().map(|s| string_width(&s.text)).sum()
}

fn tagged_width(spans: &[Tagged]) -> usize {
    spans.iter().map(|(s, _)| string_width(&s.text)).sum()
}

/// 从后面截到 `room` 格，末尾 `…`；截了一半的那一段仍去原来的地方。
fn clip(spans: Vec<Tagged>, room: usize) -> Vec<Tagged> {
    let mut out = Vec::new();
    let mut used = 0;
    for (span, target) in spans {
        let w = string_width(&span.text);
        if used + w < room {
            used += w;
            out.push((span, target));
            continue;
        }
        // 修复：这一段刚好写满 `room` 时 `truncate` 不加 `…`，后面的部分悄悄不见了；这里少给一格，让它带上 `…`（走到这里后面总还有字）。
        let text = super::lines::truncate(
            &span.text,
            room.saturating_sub(used).min(w.saturating_sub(1)),
        );
        out.push((Span::new(text, span.style), target));
        break;
    }
    out
}

/// 段的目标连成可点的地方：相邻、同一个目标的段并成一处（起始列，宽度，目标）。
fn regions(spans: &[Tagged]) -> Vec<(usize, usize, Target)> {
    let mut out: Vec<(usize, usize, Target)> = Vec::new();
    let mut col = 0;
    for (span, target) in spans {
        let w = string_width(&span.text);
        if let Some(target) = target {
            match out.last_mut() {
                Some((at, width, last)) if last == target && *at + *width == col => *width += w,
                _ => out.push((col, w, target.clone())),
            }
        }
        col += w;
    }
    out
}

impl App {
    /// 此刻的数（tui-tasks.md §2.1）。
    pub fn task_counts(&self) -> TaskCounts {
        let entries = &self.snapshots.agents.entries;
        let by_id: HashMap<&AgentId, &SubagentEntry> =
            entries.iter().map(|entry| (&entry.agent, entry)).collect();
        let passed = self
            .snapshots
            .agents
            .at
            .map_or(0, |at| self.snapshots.agents.shown.saturating_sub(at));
        let forest = Forest::new(entries, passed, self.viewing.as_ref().map(|v| &v.agent));
        let live: Vec<&SubagentEntry> = entries.iter().filter(|e| !forest.evicted(e)).collect();
        let commands = self.shell_rows();
        let monitors = commands
            .iter()
            .filter(|e| e.kind == BackgroundKind::Monitor)
            .count();
        let mut counts = TaskCounts {
            waiting: live.iter().filter(|e| e.waiting).count(),
            running: live
                .iter()
                .filter(|e| {
                    !e.waiting
                        && (e.status == SubagentStatus::Running || e.parked)
                        && in_background(e, &by_id)
                })
                .count(),
            shells: commands.len() - monitors,
            monitors,
            todos: super::tasks::todo_counts(self),
            ..TaskCounts::default()
        };
        for status in self.ended_unreported() {
            match status {
                SubagentStatus::Failed => counts.errored += 1,
                SubagentStatus::Killed => counts.stopped += 1,
                SubagentStatus::Completed | SubagentStatus::Running => counts.finished += 1,
            }
        }
        counts
    }

    /// 这一帧记下一行出现与记号换了的时刻（`draw::draw` 在画之前调）。
    pub(super) fn note_line(&mut self, now: Instant) {
        let mark = self.task_counts().mark();
        let seen = &mut self.line_seen;
        match (mark, seen.mark) {
            (None, _) => *seen = LineSeen::default(),
            (Some(mark), None) => {
                *seen = LineSeen {
                    at: Some(now),
                    mark: Some(mark),
                    mark_at: None,
                }
            }
            (Some(mark), Some(was)) if mark != was => {
                seen.mark = Some(mark);
                seen.mark_at = Some(now);
            }
            _ => {}
        }
    }

    /// 一行在动：刚出现、记号在渐变，或 `○` 借着工作光标的呼吸（runner 按动画节拍重画）。
    pub(super) fn line_moving(&self, now: Instant) -> bool {
        let young = |at: Instant| ticks_since(at, now) < TICKS;
        let seen = &self.line_seen;
        seen.at.is_some_and(young) || seen.mark_at.is_some_and(young) || self.line_breathes()
    }

    /// 回合不在跑（屏幕上没有工作光标）而有在跑的后台工作：`○` 借工作光标的呼吸。
    fn line_breathes(&self) -> bool {
        let counts = self.task_counts();
        !self.running()
            && counts.mark() == Some(LineMark::Running)
            && (counts.running > 0 || counts.commands() > 0)
    }

    /// 标题行（宽 `width`，`now` 是这一帧的动画时钟，tui-tasks.md §2.2、§2.3）：左边任务的计数（`tasks` 为假时不写，开着页），
    /// 右边 `name`；两边都没有字时没有。左边可点的地方按画出来的样子记下（§2.2「点击」），^T 不起作用时没有。
    pub fn title_row(
        &self,
        width: usize,
        now: Instant,
        name: Option<Title>,
        tasks: bool,
    ) -> Option<TitleRow> {
        let end = edge(width);
        let counts = self.task_counts();
        let (mut row, clicks) = match counts.mark().filter(|_| tasks) {
            Some(mark) => self.summary(mark, &counts, end, name.as_ref(), now),
            None => (Vec::new(), Vec::new()),
        };
        if let Some(name) = name {
            let room = end.saturating_sub(width_of(&row) + NAME_GAP);
            if room >= NAME_MIN || (name.stays && room > 0) {
                let shown = super::lines::truncate(&name.text, room);
                let pad = end.saturating_sub(width_of(&row) + string_width(&shown));
                row.push(Span::plain(" ".repeat(pad)));
                row.push(Span::new(shown, name.style));
            }
        }
        (!row.is_empty()).then(|| TitleRow {
            line: StyledLine::new(row),
            clicks,
        })
    }

    /// 标题行的左边：` 记号 部分 · 部分`，空三格接 `↓/^T all tasks`，与它可点的地方。给 `name` 留的地方见 §2.2「窄的时候」。
    fn summary(
        &self,
        mark: LineMark,
        counts: &TaskCounts,
        end: usize,
        name: Option<&Title>,
        now: Instant,
    ) -> (Vec<Span>, Vec<(usize, usize, Target)>) {
        let theme = &self.ink.theme;
        let mut glyph = match mark {
            LineMark::Waiting => Span::new("●", Style::fg(theme.code)),
            LineMark::Errored => Span::new("✗", Style::fg(theme.err)),
            LineMark::Finished => Span::new("✓", Style::fg(theme.ok)),
            LineMark::Stopped => Span::new("–", theme.fg3),
            LineMark::Running if self.line_breathes() => {
                // 呼吸按墙上时间数 tick（同以前的坞）；`shown` 是这一帧的时刻（draw::draw 写下）。
                let tick = i64::try_from(self.snapshots.agents.shown / TICK_MS).unwrap_or_default();
                Span::new("○", caret_style(theme, tick))
            }
            LineMark::Running | LineMark::Plan => Span::new("○", theme.fg3),
        };
        if let Some(at) = self.line_seen.mark_at {
            let age = ticks_since(at, now);
            glyph.style = fade::ink(theme, theme.line.fg_color(), glyph.style, age, TICKS);
        }
        let parts = parts(theme, counts, self.lang);
        let word = match self.lang {
            Lang::En => " all tasks",
            Lang::Zh => " 全部任务",
        };
        // 键不起作用时（§3.1 的条件）换成 quiet，位置不变：一行不跟着打字动。
        let usable = self.tasks_key_works();
        let (key_ink, word_ink) = if usable {
            (theme.fg2, theme.fg3)
        } else {
            (theme.quiet, theme.quiet)
        };
        // 记号与 `↓/^T all tasks` 同按 ^T，部分落在它的种类；空格与 ` · ` 不能点。
        let card = Some(Target::Tasks(None));
        let gap = |text: &'static str| (Span::plain(text), None);
        let lead = || vec![gap(" "), (glyph.clone(), card.clone())];
        // `↓/` 写出空输入框上的 ↓ 也打开同一张卡（2026-11-03 用户报告：不知道 ↓ 能直接进子代理）；窄时与 ` all tasks`
        // 一起去掉，只留 `^T`。
        let tail = |bare: bool| {
            let mut spans = vec![gap("   ")];
            if !bare {
                spans.push((Span::new("↓", key_ink), card.clone()));
                spans.push((Span::new("/", word_ink), card.clone()));
            }
            spans.push((Span::new("^T", key_ink), card.clone()));
            if !bare {
                spans.push((Span::new(word, word_ink), card.clone()));
            }
            spans
        };
        let tally = |short: &[bool]| {
            let mut spans = lead();
            spans.push(gap(" "));
            for (i, part) in parts.iter().enumerate() {
                if i > 0 {
                    spans.push((Span::new(" · ", theme.line), None));
                }
                let target = Some(Target::Tasks(Some(part.kind)));
                let shown = if short[i] { &part.short } else { &part.full };
                spans.extend(shown.iter().map(|span| (span.clone(), target.clone())));
            }
            spans
        };
        let compose = |short: &[bool], bare: bool| {
            let mut spans = tally(short);
            spans.extend(tail(bare));
            spans
        };
        // 给右边留 `wants` 列：放不下时从右往左一个一个换成短写，再去掉 `↓/` 与 ` all tasks`。
        let fit = |wants: usize| {
            let mut short = vec![false; parts.len()];
            let mut bare = false;
            while tagged_width(&compose(&short, bare)) + wants > end {
                if let Some(i) = (0..parts.len()).rev().find(|i| !short[*i]) {
                    short[i] = true;
                } else if !bare {
                    bare = true;
                } else {
                    break;
                }
            }
            (short, bare)
        };
        let wants = name.map_or(0, |n| NAME_GAP + string_width(&n.text).min(NAME_WANTS));
        let (mut short, mut bare) = fit(wants);
        let stays = name.is_some_and(|n| n.stays);
        let room = |line: &[Tagged]| end.saturating_sub(tagged_width(line) + NAME_GAP);
        // 名字反正放不下、也不是非留不可时不必为它缩短。
        if !stays && room(&compose(&short, bare)) < NAME_MIN {
            (short, bare) = fit(0);
        }
        let mut line = compose(&short, bare);
        if stays && room(&line) < LABEL_MIN {
            // 子代理的描述不走开，计数先走（§2.3）。
            line = lead();
            line.extend(tail(true));
        } else if tagged_width(&line) > end {
            // 怎么短都放不下：计数从后面截，`^T` 不去。
            line = clip(tally(&short), end.saturating_sub(tagged_width(&tail(true))));
            line.extend(tail(true));
        }
        let clicks = if usable { regions(&line) } else { Vec::new() };
        // 这一行出现时在 4 个 tick 内从背景色淡入（同以前的坞）；只淡左边，名字本来就在。
        let age = self.line_seen.at.map_or(TICKS, |at| ticks_since(at, now));
        let spans = line.into_iter().map(|(span, _)| span).collect();
        (fade::fade_in(theme, spans, age, TICKS, None), clicks)
    }
}

#[cfg(test)]
#[path = "task_line_tests.rs"]
pub(super) mod tests;

#[cfg(test)]
#[path = "task_line_click_tests.rs"]
mod click_tests;
