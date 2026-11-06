//! 终端通知（docs/specs/rust-cli/tui-manage.md §5；照 Puffin 壳的 `asks.rs` 的 `asked` 与
//! `naming.rs` 的 `window_title`）：什么时候发、发到哪个渠道（只看终端给出的事实）、写出什么转义序列、窗口标题。
//!
//! - 终端不在前台时（见过失去焦点、还没回来），每个新到的请求、每个该响的通知（定时任务的触发、同伴发来的消息、目标的
//!   结论，[`rings`]）响一次铃（BEL），再按终端发 OSC 9 / 99 / 777，窗口标题前加 `● `；请求都离开、到达过的通知也看到了
//!   （回到终端）之后恢复标题（以前是请求开着 6 s 没有按键才发，没有铃与标题，通知不响）。
//! - 回合结束后空闲 60 s 的提醒是旧界面多出的（tui-kit.md §4.3），照旧。
//!
//! 计时、焦点与“上一次按键”都是界面状态，不同步任何业务状态。
use std::collections::BTreeMap;

use rust_cli_kernel_api::interaction::NOTIFY_AFTER as RECHECK;
use rust_cli_kernel_api::orchestration::InteractionRequest;
use rust_cli_tui_kit::render::ansi::{BELL, POP_TITLE, PUSH_TITLE, set_title};
use tokio::time::{Duration, Instant};

use super::i18n::Lang;
use crate::view::{Body, PeerRow, Row};

/// 空闲提醒的缺省门槛。
pub const IDLE_AFTER: Duration = Duration::from_secs(60);
/// 通知的标题。
const TITLE: &str = "ZCode";

/// 能选到的渠道。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    ITerm2,
    Kitty,
    Ghostty,
}

/// 按终端选渠道；Apple Terminal 要起进程读响铃设置，这里不发（D15）。
pub fn channel(vars: &BTreeMap<String, String>) -> Option<Channel> {
    match vars
        .get(rust_cli_base::names::env::TERM_PROGRAM)
        .map(String::as_str)
    {
        Some("iTerm.app") => Some(Channel::ITerm2),
        Some("kitty") => Some(Channel::Kitty),
        Some("ghostty") => Some(Channel::Ghostty),
        _ if vars.contains_key(rust_cli_base::names::env::KITTY_WINDOW_ID) => Some(Channel::Kitty),
        _ => None,
    }
}

/// tmux 里 ESC 加倍后包进 DCS，screen 里直接包进 DCS。
fn wrap(sequence: String, vars: &BTreeMap<String, String>) -> String {
    if vars.contains_key(rust_cli_base::names::env::TMUX) {
        return format!("\x1bPtmux;{}\x1b\\", sequence.replace('\x1b', "\x1b\x1b"));
    }
    if vars.contains_key(rust_cli_base::names::env::STY) {
        return format!("\x1bP{sequence}\x1b\\");
    }
    sequence
}

/// kitty 通知的 `i=` 的个数（0–9999）。
const KITTY_IDS: u64 = 10_000;

/// kitty 通知的 `i=`：由一个新的随机 ID 折成 0–9999（FNV-1a）。
pub fn id(seed: &str) -> u32 {
    let hash = rust_cli_base::hash::fnv1a_64(seed.as_bytes());
    u32::try_from(hash % KITTY_IDS).unwrap_or(0)
}

/// 这个渠道要写到终端的全部字节。
pub fn sequence(
    channel: Channel,
    message: &str,
    vars: &BTreeMap<String, String>,
    id: u32,
) -> String {
    match channel {
        Channel::ITerm2 => wrap(format!("\x1b]9;\n\n{message}\x07"), vars),
        // kitty 的 OSC 用 ST 结尾（按终端选结尾）。
        Channel::Kitty => [
            format!("\x1b]99;i={id}:d=0:p=title;{TITLE}\x1b\\"),
            format!("\x1b]99;i={id}:p=body;{message}\x1b\\"),
            format!("\x1b]99;i={id}:d=1:a=focus;\x1b\\"),
        ]
        .into_iter()
        .map(|part| wrap(part, vars))
        .collect(),
        Channel::Ghostty => wrap(format!("\x1b]777;notify;{TITLE};{message}\x07"), vars),
    }
}

/// 一次通知：文字与通知的种类。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Due {
    pub message: &'static str,
    pub kind: &'static str,
}

/// 请求到达时的说法（Puffin `notify::ask_message`）。
pub fn ask_text(request: &InteractionRequest, lang: Lang) -> &'static str {
    match (lang, request) {
        (Lang::En, InteractionRequest::Permission { .. }) => "ZCode needs your permission",
        (Lang::En, InteractionRequest::PlanApproval { .. }) => {
            "ZCode needs your approval for the plan"
        }
        (Lang::En, InteractionRequest::Question { .. }) => "ZCode needs your input",
        (Lang::Zh, InteractionRequest::Permission { .. }) => "ZCode 需要你的许可",
        (Lang::Zh, InteractionRequest::PlanApproval { .. }) => "ZCode 需要你批准计划",
        (Lang::Zh, InteractionRequest::Question { .. }) => "ZCode 需要你的输入",
    }
}

/// 通知（不是请求）响铃时的说法（Puffin `NOTICE_MESSAGE`）。
pub fn notice_text(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "ZCode has news for you",
        Lang::Zh => "ZCode 有新消息",
    }
}

/// 终端在后台时它的到达响铃（Puffin `notices::rings` 与 `goals::rings`）：定时任务的触发、后台子代理回来了、同伴发来的消息（回执不响）、
/// 目标达成或判为不可能。系统提示不响。
pub fn rings(body: &Body) -> bool {
    match body {
        Body::Fire(_) | Body::Returned { .. } => true,
        Body::Peer(peer) => matches!(peer.as_ref(), PeerRow::Message { .. }),
        Body::Goal(status) => status.met || status.failed,
        _ => false,
    }
}

fn idle_text(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "ZCode is waiting for your input",
        Lang::Zh => "ZCode 正在等你输入",
    }
}

#[derive(Debug, Clone)]
struct Idle {
    finished: Instant,
    next: Instant,
    fired: bool,
}

/// 通知的界面状态。
#[derive(Debug, Clone)]
pub struct Notify {
    last_input: Option<Instant>,
    idle: Option<Idle>,
    /// 本次运行发过输入。
    submitted: bool,
    /// 终端有焦点（终端报告之前当作有，Puffin `ShellState::focused`）。
    focused: bool,
    /// 窗口标题前有 `● `（Puffin `titled`）。
    titled: bool,
    /// 终端在后台时到过该响的通知，还没回到终端（Puffin `notice_unseen`）：标题的 `●` 留着。
    unseen: bool,
    /// 看过的行：会话与其中最大的行 id。换了会话（打开、切换）时只记下，不响：那些行不是新到的。
    rows_seen: Option<(Option<String>, u64)>,
    /// 上次写到终端的标题；没写过（终端原来的标题）为 None。
    title: Option<String>,
}

impl Default for Notify {
    fn default() -> Self {
        Self {
            last_input: None,
            idle: None,
            submitted: false,
            focused: true,
            titled: false,
            unseen: false,
            rows_seen: None,
            title: None,
        }
    }
}

impl Notify {
    /// 终端输入事件（按键、鼠标、粘贴）。
    pub fn input(&mut self, now: Instant) {
        self.last_input = Some(now);
    }

    pub fn submitted(&mut self) {
        self.submitted = true;
    }

    /// 终端报告焦点（Puffin 的 `FocusIn` / `FocusOut`）：回到终端、没有请求开着时恢复标题。
    pub fn focus(&mut self, focused: bool, asks_open: bool) {
        self.focused = focused;
        if focused {
            self.unseen = false;
            if !asks_open {
                self.titled = false;
            }
        }
    }

    /// 视图里新到的行有没有该响的（[`rings`]）；记下看到了哪一行。`session` 换了时只记下。
    pub fn fresh_rows(&mut self, session: Option<&str>, rows: &[Row]) -> bool {
        let last = rows.iter().map(|row| row.id).max().unwrap_or(0);
        let seen = match &self.rows_seen {
            Some((s, id)) if s.as_deref() == session => Some(*id),
            _ => None,
        };
        self.rows_seen = Some((session.map(str::to_owned), last.max(seen.unwrap_or(0))));
        seen.is_some_and(|seen| {
            rows.iter()
                .filter(|row| row.id > seen)
                .any(|row| rings(&row.body))
        })
    }

    /// 视图变化之后（Puffin `asked`）：`fresh` 是刚到的请求，`noticed` 是新到的行里有该响的通知。终端不在前台时响铃，
    /// 返回要发的说法（请求的优先）；请求都离开、到达过的通知也看到了之后恢复标题。
    pub fn asked(
        &mut self,
        fresh: &[&InteractionRequest],
        noticed: bool,
        asks_open: bool,
        lang: Lang,
    ) -> Option<&'static str> {
        let away = !self.focused;
        let notice_rang = away && noticed;
        let ring = (away && !fresh.is_empty()) || notice_rang;
        self.unseen = self.unseen || notice_rang;
        let unmark = !asks_open && !self.unseen;
        self.titled = (self.titled || ring) && !unmark;
        ring.then(|| {
            fresh
                .first()
                .map_or_else(|| notice_text(lang), |r| ask_text(r, lang))
        })
    }

    /// 离开界面时：写过标题就弹回终端原来的。
    pub fn restore_title(&mut self) -> Option<&'static str> {
        self.title.take().map(|_| POP_TITLE)
    }

    /// 要写到终端的标题字节（Puffin `window_titled`）：想要的与上次写的不同才写；第一次写之前推下终端原来的标题，不要了
    /// 时弹回去。`name` 是会话的标题（可为空）。
    pub fn title_bytes(&mut self, name: &str) -> Option<String> {
        let name = name.trim();
        let want = if self.titled {
            Some(format!("● {}", if name.is_empty() { TITLE } else { name }))
        } else {
            (!name.is_empty()).then(|| name.to_owned())
        };
        if want == self.title {
            return None;
        }
        let out = match &want {
            Some(t) if self.title.is_none() => format!("{PUSH_TITLE}{}", set_title(t)),
            Some(t) => set_title(t),
            None => POP_TITLE.to_owned(),
        };
        self.title = want;
        Some(out)
    }

    /// 一个回合结束：空闲通知重新计时。
    pub fn turn_finished(&mut self, now: Instant) {
        self.idle = Some(Idle {
            finished: now,
            next: now + IDLE_AFTER,
            fired: false,
        });
    }

    /// 下一次要检查的时刻；没有要检查的为 None。
    pub fn next_due(&self) -> Option<Instant> {
        self.idle
            .as_ref()
            .filter(|idle| !idle.fired && self.submitted)
            .map(|idle| idle.next)
    }

    /// 到点的检查：返回要发的空闲提醒。`quiet` 是空闲通知的其余条件（没有在跑的回合、待处理交互、打开的页面与
    /// 在跑的后台子代理）。
    pub fn due(&mut self, now: Instant, quiet: bool, lang: Lang) -> Option<Due> {
        let last_input = self.last_input;
        let submitted = self.submitted;
        if let Some(idle) = self
            .idle
            .as_mut()
            .filter(|idle| !idle.fired && submitted && idle.next <= now)
        {
            // 结束之后有过按键：这次不再提醒（不再重新计时，等下一次回合结束）。
            if last_input.is_some_and(|input| input > idle.finished) {
                idle.fired = true;
                return None;
            }
            if !quiet {
                idle.next = now + RECHECK;
                return None;
            }
            idle.fired = true;
            return Some(Due {
                message: idle_text(lang),
                kind: "idle_prompt",
            });
        }
        None
    }
}

#[cfg(test)]
#[path = "notify_tests.rs"]
mod tests;

/// 铃的字节（Puffin `ansi::BELL`）。
pub const RING: &str = BELL;
