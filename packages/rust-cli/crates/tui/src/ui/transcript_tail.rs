//! transcript 的尾巴（docs/specs/rust-cli/tui-kit.md §4.0「尾巴」；Puffin `shell/view/tail.rs` 的 `tail_entries`、
//! `caret_entry`、`retry_entry`、`outgoing_entries`）：回合的活的状态在 transcript 的末尾，不在底部。
//!
//! - **工作光标**：回合在跑、没有请求等你时有它。笔骑在流式的回答末尾时就在那里（`markdown::streaming`，手停住之后是点阵）；否则单独一行，
//!   第 3 列，在下一块要来的位置。手停住之后 15 个 tick 里是呼吸的笔尖，之后是 kit `lattice::line` 的 `⋮⋮⋮ Working  7s`。
//! - **重试行**：一段重试持续 1 秒（30 个 tick）之后代替单独一行的光标（笔也不再骑在回答末尾）。
//! - **发出的消息**：插进这一回合的消息（`Delivery::Steer`）与建会话期间留下的输入画成你的回合，最后一条下面 fg3 的
//!   `Queued. …`。等下一轮的（`Delivery::Queue`）在板上的栈里。
//!
//! 以前板上面常驻一行状态行（随机的动词、用时、token 数、思考状态，卡住 10 秒后渐渐变红），排队的消息都在板上。
use rust_cli_kernel_api::session_api::Delivery;
use rust_cli_tui_kit::app::element::single_line;
use rust_cli_tui_kit::app::frame::{RETRY_GRACE_TICKS, retry_line_with};
use rust_cli_tui_kit::app::layout::Layout as Lay;
use rust_cli_tui_kit::app::widgets::entries::quiet_line;
use rust_cli_tui_kit::app::widgets::pen::{Pen, Still};
use rust_cli_tui_kit::markdown::render::INDENT;
use rust_cli_tui_kit::render::{Span, Style};

use super::{Layout, Prompt, Rendered};
use crate::ui::app::App;
use crate::ui::cells::{self, Ctx};
use crate::ui::i18n::Lang;

/// 尾巴的块的键（与行 id、会话头、本地块的键都不重）。
const TAIL_KEY: u64 = u64::MAX - (1 << 32);

/// 尾巴的时钟（存在排版缓存里，帧与帧之间留着）：手停住的那个 tick 与这一串重试开始的 tick（Puffin `wait_since`、
/// `retry_since`）。
#[derive(Default)]
pub struct Clocks {
    wait_since: Option<i64>,
    retry_since: Option<i64>,
    /// 工作光标上次出现的 tick 与上一帧它在不在：呼吸从它出现时数起（Puffin 在 `caret_on` 变真时把 `frame` 归零）。
    caret_from: i64,
    caret_was: bool,
}

/// 这一帧的尾巴怎样画：笔（回合的工作光标在不在）、重试行要说的（显示时）、回合在不在跑。
pub struct Tail {
    pub pen: Option<Pen>,
    retry: Option<(String, String, Option<String>)>,
    turn: bool,
}

impl Tail {
    /// 没有重试行代替光标。
    pub fn retry_free(&self) -> bool {
        self.retry.is_none()
    }
}

/// 有请求等你（光标让给它，Puffin `caret_on` 的 `!asking`）。
fn asking(app: &App) -> bool {
    app.viewing.is_none() && (app.pending().is_some() || app.view.peer_held().is_some())
}

/// 回合在跑：主会话的，查看子代理时是它的（修复线 F112）。
fn turn(app: &App) -> bool {
    match &app.viewing {
        Some(_) => app.viewed_entry().is_some_and(|entry| {
            entry.status == rust_cli_kernel_api::panels::SubagentStatus::Running
        }),
        None => app.running(),
    }
}

/// 回合开始到现在的毫秒（工作行写的时间）：主会话按回合开始的时刻，查看子代理时按它的用时。
fn turn_ms(app: &App, now_ms: u64) -> f64 {
    match &app.viewing {
        Some(_) => app.viewed_entry().map_or(0.0, |entry| {
            crate::ui::agents::elapsed_ms(&app.snapshots.agents, entry) as f64
        }),
        None => app
            .view
            .turn_started_ms()
            .map_or(0.0, |at| now_ms.saturating_sub(at) as f64),
    }
}

/// 帧开始时定下尾巴：重试这一串从哪个 tick 起（满 [`RETRY_GRACE_TICKS`] 才显示）、笔在不在。手停了多久在排完之后定
/// （[`settle`]：要看这一帧的播放头）。`typing`：回合的文字还在打出，回合结束了笔也还在（Puffin `caret_on` 的 `typing`）。
pub fn begin(app: &App, clocks: &mut Clocks, tick: i64, now_ms: u64, typing: bool) -> Tail {
    let turn = turn(app);
    let notice = app.viewing.is_none().then(|| app.view.notice()).flatten();
    let words = notice
        .filter(|_| turn)
        .and_then(|n| crate::ui::retry_status::tail_words(n, now_ms, app.lang));
    clocks.retry_since = words.as_ref().map(|_| clocks.retry_since.unwrap_or(tick));
    let retry = words.filter(|_| {
        clocks
            .retry_since
            .is_some_and(|t| tick - t >= RETRY_GRACE_TICKS)
    });
    let pen = ((turn || typing) && !asking(app)).then(|| Pen {
        theme: app.ink.theme,
        frame: tick,
        still: clocks.wait_since.map(|w| Still {
            age: tick - w,
            turn_ms: turn_ms(app, now_ms),
        }),
        held: false,
    });
    Tail { pen, retry, turn }
}

/// 这一帧笔在不在定了之后（esc 停下时拿掉笔之后）：光标出现的那一帧呼吸从最亮开始（Puffin `post` 的 `state.frame = 0`，
/// docs/puffin-tui.md §4.2「笔」），之后按 tick 往下数；光标不在时照样数，下次出现再归零。以前呼吸按绝对的 tick，光标出现时
/// 的亮度随它出现的时刻变（twin subagent-finished：内核开的回合第一帧的 `▌` 比 Puffin 暗）。
pub fn breathe(tail: &mut Tail, clocks: &mut Clocks, tick: i64) {
    let on = tail.pen.is_some();
    if on && !clocks.caret_was {
        clocks.caret_from = tick;
    }
    clocks.caret_was = on;
    if let Some(pen) = tail.pen.as_mut() {
        pen.frame = tick - clocks.caret_from;
    }
}

/// 排完之后：生命迹象可以显示（笔在、没有重试行代替它）时，手停住的那一刻是上次的或这一刻，不早于最后一个词落下
/// （Puffin `waited`）；否则没有。`landed` 是播放头最后动的 tick。
pub fn settle(tail: &mut Tail, clocks: &mut Clocks, tick: i64, landed: i64) {
    let sign_on = tail.pen.is_some() && tail.retry.is_none();
    clocks.wait_since = sign_on.then(|| clocks.wait_since.unwrap_or(tick).max(landed));
    if let Some(pen) = tail.pen.as_mut() {
        pen.still = clocks.wait_since.map(|w| Still {
            age: tick - w,
            turn_ms: pen.still.map_or(0.0, |s| s.turn_ms),
        });
    }
}

/// 上一块在间距里的样子：单独一行的光标或重试行上面空不空一行（Puffin `tail_entries` 的 `gap`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Last {
    /// 没有你的回合。
    Nothing,
    /// 板的下沿（你的消息、`!` 命令）：plain 主题空一行。
    Slab,
    /// 工作行与开启一轮的行、还在流式的回答：不空（下一块可能挂着）。
    Hanging,
    /// 说完的话：空一行。
    Said,
}

/// 尾巴的块接在 `out` 后面：回合在跑时先是单独一行的光标（或重试行）再是发出的消息，否则反过来。`rode`：笔骑在回答末尾。
pub fn push(app: &App, out: &mut Layout, tail: &Tail, last: Last, rode: bool, ctx: &Ctx<'_>) {
    let theme = &ctx.ink.theme;
    let outgoing = outgoing(app);
    let shells = app.view.held_shells();
    let under_slab =
        (!tail.turn && !outgoing.is_empty()) || !shells.is_empty() || last == Last::Slab;
    let gap = if under_slab {
        !theme.rich
    } else {
        last == Last::Said
    };
    let free = tail.pen.filter(|p| !p.held);
    let alone = if rode {
        None
    } else {
        match (&tail.retry, free) {
            (Some((reason, wait, count)), pen) => Some(Lay::lay(
                &retry_line_with(
                    theme,
                    reason.clone(),
                    wait.clone(),
                    count.clone(),
                    pen.map_or(theme.fg3, |p| p.caret()),
                ),
                ctx.width,
            )),
            (None, Some(pen)) => {
                let mut spans = vec![Span::new(INDENT, Style::DEFAULT)];
                spans.extend(pen.waiting());
                Some(Lay::lay(&single_line(spans, false), ctx.width))
            }
            (None, None) => None,
        }
    };
    let push_alone = |out: &mut Layout| {
        if let Some(lines) = alone.clone() {
            out.push_spaced(TAIL_KEY, Rendered::new(lines), None, gap);
        }
    };
    push_shells(out, shells, last, ctx);
    if tail.turn {
        push_alone(out);
        push_outgoing(app, out, &outgoing, true, last, ctx);
    } else {
        push_outgoing(app, out, &outgoing, false, last, ctx);
        push_alone(out);
    }
}

/// 发出了、还没到 agent 的消息：插进这一回合的（`Delivery::Steer`）与建会话期间留下的输入（Puffin `outgoing` 里 `!after`
/// 的那些）。
pub fn outgoing(app: &App) -> Vec<String> {
    // 还没认领的输入也在这里（`view/in_flight.rs`）：插话与空闲时开回合的消息按键的那一帧就在尾巴里。
    let outgoing = app.view.outgoing();
    let steer = outgoing
        .iter()
        .filter(|item| item.delivery == Delivery::Steer)
        .map(|item| item.text.clone());
    // 留下的 `!` 命令画成在跑的块（[`push_shells`]），不是你的回合。
    let held = app
        .held
        .iter()
        .filter(|command| {
            !matches!(
                command,
                rust_cli_kernel_api::session_api::Command::Bash { .. }
            )
        })
        .filter_map(crate::ui::bottom::held_text);
    steer
        .chain(held)
        .map(|text| {
            rust_cli_kernel_api::user_input::display(&text)
                .unwrap_or(text)
                .trim()
                .to_owned()
        })
        .collect()
}

/// 内核还没写下的 `!` 命令：在它的块将要在的地方（transcript 的末尾）画成在跑的块，还没有输出（Puffin `held_entries`）。
fn push_shells(out: &mut Layout, shells: &[String], last: Last, ctx: &Ctx<'_>) {
    let theme = &ctx.ink.theme;
    for (i, command) in shells.iter().enumerate() {
        let gap = if i > 0 {
            !theme.rich
        } else {
            match last {
                Last::Nothing => false,
                Last::Slab => !theme.rich,
                Last::Hanging | Last::Said => true,
            }
        };
        let key = TAIL_KEY - (1 << 16) - i as u64;
        out.push_spaced(
            key,
            Rendered::new(cells::shell_input(command, ctx)),
            None,
            gap,
        );
    }
}

/// 发出的消息画成你的回合，最后一条下面是 fg3 的 `Queued. …`（Puffin `outgoing_entries`）。`after_caret`：在光标之后。
fn push_outgoing(
    app: &App,
    out: &mut Layout,
    lane: &[String],
    after_caret: bool,
    last: Last,
    ctx: &Ctx<'_>,
) {
    let theme = &ctx.ink.theme;
    let zh = app.lang == Lang::Zh;
    // 没有模型（门道开着）时留下的输入（runner `submit`，Puffin `outgoing_entries` 的 `needs_model`）。缺省模型还没
    // 定下时不算没有模型：定下后就发（performance.md §12.3）。
    let why = if app.view.selection().model.is_empty()
        && !app.held.is_empty()
        && !app.creating
        && !app.model_pending
    {
        Some(if zh {
            "已排队。接上模型后发出。"
        } else {
            "Queued. It goes out once a model is connected."
        })
    } else if !app.held.is_empty() || app.creating {
        Some(if zh {
            "已排队。agent 准备好后马上发出。"
        } else {
            "Queued. It goes out as soon as the agent is ready."
        })
    } else if app.running() {
        Some(if zh {
            "已排队。agent 在下一步读到它。"
        } else {
            "Queued. The agent reads it at its next step."
        })
    } else {
        None
    };
    let end = lane.len().saturating_sub(1);
    for (i, text) in lane.iter().enumerate() {
        let gap = if i > 0 {
            !theme.rich
        } else {
            after_caret
                || match last {
                    Last::Nothing => false,
                    Last::Slab => !theme.rich,
                    Last::Hanging | Last::Said => true,
                }
        };
        let mut lines = cells::user(text, ctx);
        if i == end
            && let Some(why) = why
        {
            lines.extend(Lay::lay(&quiet_line(theme, why), ctx.width));
        }
        let key = TAIL_KEY - 1 - i as u64;
        if let Some(start) = out.push_spaced(key, Rendered::new(lines), None, gap) {
            out.prompts.push(Prompt {
                start: super::compose::entry_start(start, gap),
                end: out.len(),
                row_id: key,
                text: text.clone(),
            });
        }
    }
}
