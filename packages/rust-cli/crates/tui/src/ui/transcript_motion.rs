//! transcript 的动效的时钟（docs/puffin-tui.md §4.2「工具调用」「流式节奏」「通知」；Puffin `shell/app/clocks.rs` 的
//! `new_clocks`、`shell/app/session.rs` 的 `new_playheads` / `new_notices`）：每个调用出现、结束、一段读与一段工作关上的
//! tick，回答与思考的播放头，通知与失败块到达的 tick。存在排版缓存里，按视图行的 id。
//!
//! Puffin 在每次增量到达时比较前后的投影记下这些 tick；旧界面在画每一帧时比较这一帧与上一帧见到的行（行有 id 与版本），
//! 记下同样的 tick。冷加载的行（[`crate::view::Transcript::cold_until`] 之前）照它们的样子画：没有出现的动效，已有的文字
//! 不重放。动效走完之后时钟去掉（走完的时钟与没有时钟画得一样）。
use std::collections::{HashMap, HashSet};

use rust_cli_tui_kit::app::widgets::fade;
use rust_cli_tui_kit::app::widgets::playhead::Playhead;
use rust_cli_tui_kit::app::widgets::tool_row::ToolClock;

use crate::view::{Body, Row};

/// 调用的动效走完要的 tick（Puffin `consts::TOOL_MOTION_TICKS`）。
pub const TOOL_MOTION_TICKS: i64 = 8;
/// 播放头一帧至多追这么多 tick（帧之间隔得久时不一口气走很多步）。
const CATCH_UP: i64 = 30;

/// 上一帧见到的一行：结束了没有。
#[derive(Clone, Copy)]
struct Seen {
    ended: bool,
}

/// 一段工作（或展开时一段读）的头在上一帧关没关上。
#[derive(Default)]
pub struct Motion {
    seen: HashMap<u64, Seen>,
    /// 一段工作的头 → 关上了没有（收起的样子）；一段读的头 → 关上了没有（展开的样子）。
    runs: HashMap<u64, bool>,
    reads: HashMap<u64, bool>,
    clocks: HashMap<u64, ToolClock>,
    notices: HashMap<u64, i64>,
    /// 回答与思考的播放头与它走到的 tick。
    heads: HashMap<u64, (Playhead, i64)>,
    /// 这一帧之前见过帧（清空之后的第一帧不算有“上一帧”）。
    started: bool,
    /// 关上了、里面的一段读也都关上了的一段工作（按头）：不再看它。
    settled: HashSet<u64>,
}

/// 行的文字（回答与思考）。
fn text_of(row: &Row) -> Option<(&str, bool)> {
    match &row.body {
        Body::Agent { text, streaming }
        | Body::Reasoning {
            text, streaming, ..
        } => Some((text.as_str(), !*streaming)),
        _ => None,
    }
}

/// 行结束了没有（调用不再在跑；别的行总是结束了的）。
fn ended(row: &Row) -> bool {
    match &row.body {
        Body::Tool(tool) => !tool.status.is_live(),
        _ => true,
    }
}

impl Motion {
    /// 换了视图（`/clear`、换会话）：全部的时钟作废。
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// 这一帧见到的行（tick `now`）：新的调用现在出现、在跑的变成结束的现在结束；新的回答与思考从头打出（快照里还在流的
    /// 从末尾）；新的通知与失败块现在淡入。播放头走到 `now`。
    pub fn observe(&mut self, rows: &[Row], cold: u64, now: i64) {
        let mut seen = HashMap::with_capacity(rows.len());
        for row in rows {
            let was = self.seen.get(&row.id).copied();
            let fresh = was.is_none() && row.id >= cold && self.started;
            let done = ended(row);
            if matches!(row.body, Body::Tool(_)) {
                match was {
                    None if fresh => {
                        let clock = self.clocks.entry(row.id).or_default();
                        clock.arrived = Some(now);
                        clock.ended = done.then_some(now);
                    }
                    Some(w) if !w.ended && done => {
                        self.clocks.entry(row.id).or_default().ended = Some(now);
                    }
                    _ => {}
                }
            }
            if fresh
                && matches!(
                    row.body,
                    Body::Notice { .. }
                        | Body::Failure(_)
                        | Body::Fire(_)
                        | Body::Returned { .. }
                        | Body::Peer(_)
                        | Body::ShellOutput { .. }
                )
            {
                self.notices.entry(row.id).or_insert(now);
            }
            if let Some((text, fin)) = text_of(row)
                && was.is_none()
                && !self.heads.contains_key(&row.id)
            {
                if fresh {
                    self.heads.insert(row.id, (Playhead::start(0), now));
                } else if !fin {
                    self.heads
                        .insert(row.id, (Playhead::start(text.len()), now));
                }
            }
            seen.insert(row.id, Seen { ended: done });
        }
        // 播放头每个 tick 走一步，按已到达的文字。
        for row in rows {
            if let (Some((head, at)), Some((text, fin))) =
                (self.heads.get_mut(&row.id), text_of(row))
            {
                let from = (*at).max(now - CATCH_UP);
                for tick in from + 1..=now {
                    *head = head.step(text, fin, tick);
                }
                *at = (*at).max(now);
            }
        }
        // 打完、落定、抬笔之后播放头不再要：照完整的样子画（Puffin 提交）。
        self.heads.retain(|id, (head, _)| {
            rows.iter()
                .find(|row| row.id == *id)
                .and_then(text_of)
                .is_some_and(|(text, fin)| !head.done(text.len(), fin, now))
        });
        self.clocks
            .retain(|id, clock| seen.contains_key(id) && moving(clock, now));
        self.notices
            .retain(|id, at| seen.contains_key(id) && now - *at < fade::TICKS);
        self.seen = seen;
        self.started = true;
    }

    /// 这一帧一段工作（按头）关上了没有：从开着变成关上、或第一次见到就关上（不是快照里的）的，现在结束（标记从这里落定）。
    /// `reads` 为真时是展开的一段读。
    pub fn closed(&mut self, head: u64, closed: bool, fresh: bool, reads: bool, now: i64) {
        let map = if reads {
            &mut self.reads
        } else {
            &mut self.runs
        };
        let was = map.insert(head, closed);
        if closed && (was == Some(false) || (was.is_none() && fresh)) {
            let clock = self.clocks.entry(head).or_default();
            if reads {
                clock.closed.get_or_insert(now);
            } else {
                clock.gathered.get_or_insert(now);
            }
        }
    }

    /// 一段工作（按头）和它里面的一段读都关上了，记下过了。
    pub fn settled(&self, head: u64) -> bool {
        self.settled.contains(&head)
    }

    /// 一段工作（按头）和它里面的一段读都关上了：之后不再看它。
    pub fn settle(&mut self, head: u64) {
        self.settled.insert(head);
    }

    /// 行 `id` 的动效时钟（没有时是空的）。
    pub fn clock(&self, id: u64) -> ToolClock {
        self.clocks.get(&id).cloned().unwrap_or_default()
    }

    /// 行 `id` 的播放头。
    pub fn head(&self, id: u64) -> Option<Playhead> {
        self.heads.get(&id).map(|(head, _)| head.clone())
    }

    /// 播放头最后一次动的 tick（手停住不早于它）。
    pub fn landed(&self) -> i64 {
        self.heads
            .values()
            .map(|(head, _)| head.moved)
            .max()
            .unwrap_or(i64::MIN)
    }

    /// 通知或失败块淡入到哪里（0 到 1；落定或没有记下时是 1）。`!` 命令的输出也记在这张表里：它结束时的渐变
    /// （`cells::shell_output`）。
    pub fn fade(&self, id: u64, now: i64) -> f64 {
        self.notices
            .get(&id)
            .map_or(1.0, |at| ((now - at) as f64 / fade::TICKS as f64).min(1.0))
    }

    /// 通知或失败块到达以来的 tick（没有记下时是很久以前）。
    pub fn notice_age(&self, id: u64, now: i64) -> Option<i64> {
        self.notices.get(&id).map(|at| now - at)
    }

    /// 行 `id` 的文字还没全部打出（Puffin `holding`：后面的条目等着）：回答，与详细的样子（`details`，^O）里的思考。
    pub fn holding(&self, row: &Row, details: bool) -> bool {
        (matches!(row.body, Body::Agent { .. })
            || (details && matches!(row.body, Body::Reasoning { .. })))
            && self
                .heads
                .get(&row.id)
                .zip(text_of(row))
                .is_some_and(|((head, _), (text, _))| head.shown < text.len())
    }

    /// 还有东西在动：按动画节拍重画（Puffin 的计时器：播放头、调用与通知的动效）。
    pub fn moving(&self) -> bool {
        !self.heads.is_empty() || !self.clocks.is_empty() || !self.notices.is_empty()
    }

    /// 行 `id` 这一帧还在动（它的块不能用缓存）。
    pub fn moving_row(&self, id: u64) -> bool {
        self.heads.contains_key(&id)
            || self.clocks.contains_key(&id)
            || self.notices.contains_key(&id)
    }
}

/// 时钟还在动效里。
fn moving(clock: &ToolClock, now: i64) -> bool {
    [
        clock.arrived,
        clock.ended,
        clock.closed,
        clock.gathered,
        clock.landed.last().map(|l| l.1),
    ]
    .into_iter()
    .flatten()
    .max()
    .is_some_and(|t| now - t < TOOL_MOTION_TICKS)
}
