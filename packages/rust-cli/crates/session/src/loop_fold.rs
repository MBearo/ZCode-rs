//! `/loop` 的 noop 折叠（docs/specs/rust-cli/tools-more.md §18.3）。
//!
//! 锚点是上一次 loop 唤醒触发开的 prompt 消息。状态只在 actor 里（与哨兵展开同一个 owner），不持久化，
//! 压缩之后清空（压缩边界本来也会让折叠不成立）。

use std::collections::HashSet;

use rust_cli_base::{civil, units};
use rust_cli_kernel_api::cron::{CronFireKind, LoopFold};
use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::message::{ContentBlock, Message, SystemMessage, UserMessage, UserOrigin};
use rust_cli_kernel_api::session_api::KernelEvent;

use crate::actor::Actor;
use crate::cron::CRON_SOURCE;
use crate::notices::NOTICE_SOURCE;

/// 给模型的折叠说明的来源（meta user 消息）。
pub(crate) const FOLD_SOURCE: &str = "loop-noop";
const SCHEDULE_WAKEUP: &str = "ScheduleWakeup";
/// 用户拒绝与中断的工具结果（内核消息没有拒绝种类的字段，按文字认，D44）。
const DENIALS: [&str; 2] = [
    "The user doesn't want to proceed with this tool use.",
    "[Request interrupted by user",
];

/// 折叠的锚点与连续次数（`noOpStreak` / `streakStartedAt`）。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct FoldState {
    /// loop 唤醒开的 prompt 消息，按触发顺序（最后一个是锚点）。
    anchors: Vec<MessageId>,
    /// 锚点的连续次数与开始时间（毫秒）。
    streak: u32,
    since_ms: u64,
}

/// 一次能折叠的结果：连续次数（含刚结束的这一个）与开始时间。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Fold {
    pub streak: u32,
    pub since_ms: u64,
}

fn is_cron_prompt(message: &Message) -> bool {
    matches!(message, Message::User(user)
        if matches!(&user.origin, UserOrigin::Meta { source } if source == CRON_SOURCE))
}

fn denied(content: &[rust_cli_kernel_api::message::ToolResultContent]) -> bool {
    content.iter().any(|item| match item {
        rust_cli_kernel_api::message::ToolResultContent::Text { text } => {
            DENIALS.iter().any(|denial| text.starts_with(denial))
        }
        _ => false,
    })
}

impl FoldState {
    /// 从锚点看到现在，上一个 tick 能不能折叠。
    pub(crate) fn analyze(&self, messages: &[Message]) -> Option<Fold> {
        let anchor = self.anchors.last()?;
        let at = messages
            .iter()
            .rposition(|message| message.id() == anchor)?;
        // 锚点之前到上一个 loop 触发之间有压缩或别的定时任务：不折叠。
        for message in messages[..at].iter().rev() {
            if self.anchors.contains(message.id()) {
                break;
            }
            if is_cron_prompt(message)
                || matches!(message, Message::System(SystemMessage::CompactBoundary(_)))
            {
                return None;
            }
        }
        let mut tools = HashSet::new();
        let mut noop = None;
        for message in &messages[at + 1..] {
            match message {
                Message::System(_) => return None,
                Message::Attachment(attachment) if attachment.source == "queued_command" => {
                    return None;
                }
                Message::Attachment(_) => {}
                Message::Assistant(assistant) => {
                    for block in &assistant.content {
                        if let ContentBlock::ToolUse { id, name, input } = block {
                            tools.insert(id.clone());
                            if name == SCHEDULE_WAKEUP {
                                noop =
                                    Some(input.get("noop") == Some(&serde_json::Value::Bool(true)));
                            }
                        }
                    }
                }
                Message::User(user) => match &user.origin {
                    UserOrigin::ToolResult { .. } => {
                        for block in &user.content {
                            if let ContentBlock::ToolResult {
                                tool_use_id,
                                content,
                                is_error,
                            } = block
                                && (!tools.contains(tool_use_id) || (*is_error && denied(content)))
                            {
                                return None;
                            }
                        }
                    }
                    // 人类输入、定时任务、后台通知（有来源的输入）都算外来输入；其余 meta 不算。
                    UserOrigin::Human | UserOrigin::CompactSummary => return None,
                    UserOrigin::Meta { source }
                        if source == CRON_SOURCE || source == NOTICE_SOURCE =>
                    {
                        return None;
                    }
                    UserOrigin::Meta { .. } => {}
                },
            }
        }
        (noop == Some(true)).then_some(Fold {
            streak: self.streak + 1,
            since_ms: self.since_ms,
        })
    }

    /// 这次触发开的 prompt 成为新锚点。
    pub(crate) fn fired(&mut self, prompt: MessageId, fold: Option<Fold>, at_ms: u64) {
        self.anchors.push(prompt);
        (self.streak, self.since_ms) = match fold {
            Some(fold) => (fold.streak, fold.since_ms),
            None => (0, at_ms),
        };
    }
}

fn plural(count: u32, word: &str) -> String {
    if count == 1 {
        word.to_owned()
    } else {
        format!("{word}s")
    }
}

/// 给模型的折叠说明。
pub(crate) fn note(streak: u32) -> String {
    format!(
        "[{streak} prior /loop {} found nothing actionable; loop is healthy.]",
        plural(streak, "wakeup")
    )
}

/// 时刻的写法：`toLocaleString("en-US", {month: short, day, hour, minute: 2-digit})` 去掉逗号、`AM` 变成
/// 紧接的小写 `am`（`Oct 18 3:04pm`）；本地时间 = UTC + 偏移分钟。
pub(crate) fn label(at_ms: u64, offset_minutes: i32) -> String {
    // 公历、月份缩写与 12 小时制只定义在 base 的 `civil`。
    let minutes =
        i64::try_from(at_ms / units::MS_PER_MINUTE).unwrap_or(0) + i64::from(offset_minutes);
    let (_, month, day) = civil::civil_from_days(minutes.div_euclid(units::MINUTES_PER_DAY as i64));
    let of_day = minutes.rem_euclid(units::MINUTES_PER_DAY as i64);
    let minutes_per_hour = units::MINUTES_PER_HOUR as i64;
    let (hour, minute) = (of_day / minutes_per_hour, of_day % minutes_per_hour);
    let (twelve, half) = civil::twelve_hour(u32::try_from(hour).unwrap_or(0));
    let month = civil::MONTH_ABBREVIATIONS[month as usize - 1];
    format!(
        "{month} {day} {twelve}:{minute:02}{}",
        half.to_ascii_lowercase()
    )
}

/// 内核事件里的折叠（开始时间写成同样的格式）。
pub(crate) fn event_fold(fold: Fold, offset: &impl Fn(u64) -> i32) -> LoopFold {
    LoopFold {
        streak: fold.streak,
        since: label(fold.since_ms, offset(fold.since_ms)),
    }
}

/// 一次触发的准备：界面行已发出；`before` 是排在 prompt 之前的消息（折叠说明）。
pub(crate) struct Fired {
    pub(crate) before: Vec<Message>,
    kind: CronFireKind,
    fold: Option<Fold>,
    at_ms: u64,
}

impl Actor {
    /// 触发时（会话空闲）：算折叠、发界面行。错过提示没有界面行。
    pub(crate) fn fire_prelude(&mut self, kind: CronFireKind) -> Fired {
        let at_ms = self.deps.clock.now_ms();
        let clock = self.deps.clock.clone();
        let offset = move |at: u64| clock.utc_offset_minutes(at);
        let fold = match kind {
            CronFireKind::Loop => self.loop_ticks.fold.analyze(&self.messages),
            CronFireKind::Task | CronFireKind::Notice => None,
        };
        if kind != CronFireKind::Notice {
            self.log.append(KernelEvent::ScheduledFire {
                at: label(at_ms, offset(at_ms)),
                kind,
                fold: fold.map(|fold| event_fold(fold, &offset)),
            });
        }
        let before = fold
            .map(|fold| {
                vec![Message::User(UserMessage {
                    id: self.deps.ids.message(),
                    content: vec![ContentBlock::Text {
                        text: note(fold.streak),
                    }],
                    origin: UserOrigin::Meta {
                        source: FOLD_SOURCE.to_owned(),
                    },
                })]
            })
            .unwrap_or_default();
        Fired {
            before,
            kind,
            fold,
            at_ms,
        }
    }

    /// prompt 已提交：loop 唤醒开的 prompt 成为新的折叠锚点。
    pub(crate) fn fired(&mut self, fired: Fired, prompt: MessageId) {
        if fired.kind == CronFireKind::Loop {
            self.loop_ticks.fold.fired(prompt, fired.fold, fired.at_ms);
        }
    }
}

#[cfg(test)]
#[path = "loop_fold_tests.rs"]
mod tests;
