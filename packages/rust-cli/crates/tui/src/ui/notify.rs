//! 终端通知（docs/specs/rust-cli/tui-manage.md §5）：什么时候发（待处理交互 6 s、回合结束后空闲 60 s）、
//! 发到哪个渠道（只看终端给出的事实）、写出什么转义序列。计时与“上一次按键”都是界面状态，不同步任何业务状态。
use std::collections::BTreeMap;

use rust_cli_kernel_api::ids::InteractionId;
use rust_cli_kernel_api::orchestration::InteractionRequest;
use tokio::time::{Duration, Instant};

use super::i18n::Lang;

/// 待处理交互的提醒间隔：只定义在
/// kernel-api，权限询问的 Notification hook 用同一个值（F58：以前这里与 session 各写一份）。
pub use rust_cli_kernel_api::interaction::NOTIFY_AFTER as DIALOG_AFTER;
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
    match vars.get("TERM_PROGRAM").map(String::as_str) {
        Some("iTerm.app") => Some(Channel::ITerm2),
        Some("kitty") => Some(Channel::Kitty),
        Some("ghostty") => Some(Channel::Ghostty),
        _ if vars.contains_key("KITTY_WINDOW_ID") => Some(Channel::Kitty),
        _ => None,
    }
}

/// tmux 里 ESC 加倍后包进 DCS，screen 里直接包进 DCS。
fn wrap(sequence: String, vars: &BTreeMap<String, String>) -> String {
    if vars.contains_key("TMUX") {
        return format!("\x1bPtmux;{}\x1b\\", sequence.replace('\x1b', "\x1b\x1b"));
    }
    if vars.contains_key("STY") {
        return format!("\x1bP{sequence}\x1b\\");
    }
    sequence
}

/// kitty 通知的 `i=`：由一个新的随机 ID 折成 0–9999（FNV-1a）。
pub fn id(seed: &str) -> u32 {
    let hash = seed.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    u32::try_from(hash % 10_000).unwrap_or(0)
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

fn dialog_text(request: &InteractionRequest, lang: Lang) -> &'static str {
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

fn idle_text(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "ZCode is waiting for your input",
        Lang::Zh => "ZCode 正在等你输入",
    }
}

#[derive(Debug, Clone)]
struct Dialog {
    id: InteractionId,
    message: &'static str,
    shown: Instant,
    next: Instant,
    fired: bool,
}

#[derive(Debug, Clone)]
struct Idle {
    finished: Instant,
    next: Instant,
    fired: bool,
}

/// 通知的界面状态。
#[derive(Debug, Clone, Default)]
pub struct Notify {
    last_input: Option<Instant>,
    dialog: Option<Dialog>,
    idle: Option<Idle>,
    /// 本次运行发过输入。
    submitted: bool,
}

impl Notify {
    /// 终端输入事件（按键、鼠标、粘贴）。
    pub fn input(&mut self, now: Instant) {
        self.last_input = Some(now);
    }

    pub fn submitted(&mut self) {
        self.submitted = true;
    }

    /// 视图里的待处理交互（每次视图变化后）：换了交互就重新计时，没有了就停。
    pub fn dialog(
        &mut self,
        pending: Option<(&InteractionId, &InteractionRequest)>,
        lang: Lang,
        now: Instant,
    ) {
        match pending {
            None => self.dialog = None,
            Some((id, request)) => {
                if self.dialog.as_ref().is_none_or(|dialog| dialog.id != *id) {
                    self.dialog = Some(Dialog {
                        id: id.clone(),
                        message: dialog_text(request, lang),
                        shown: now,
                        next: now + DIALOG_AFTER,
                        fired: false,
                    });
                }
            }
        }
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
        let dialog = self.dialog.as_ref().filter(|d| !d.fired).map(|d| d.next);
        let idle = self
            .idle
            .as_ref()
            .filter(|idle| !idle.fired && self.submitted)
            .map(|idle| idle.next);
        match (dialog, idle) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// 到点的检查：返回要发的通知。`quiet` 是空闲通知的其余条件（没有在跑的回合、待处理交互、打开的页面与
    /// 在跑的后台子代理）。
    pub fn due(&mut self, now: Instant, quiet: bool, lang: Lang) -> Option<Due> {
        let last_input = self.last_input;
        if let Some(dialog) = self.dialog.as_mut().filter(|d| !d.fired && d.next <= now) {
            let since = last_input.map_or(dialog.shown, |input| input.max(dialog.shown));
            if now.duration_since(since) >= DIALOG_AFTER {
                dialog.fired = true;
                return Some(Due {
                    message: dialog.message,
                    kind: "permission_prompt",
                });
            }
            dialog.next = now + DIALOG_AFTER;
        }
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
                idle.next = now + DIALOG_AFTER;
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
