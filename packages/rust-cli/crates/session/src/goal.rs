//! `/goal`（docs/specs/rust-cli/goal.md）：会话 actor 是目标的唯一 owner。
//!
//! - 命令（§2）：人类输入开回合时先认出 `/goal …`，按本地命令写进对话的消息（不经 skill 展开）；
//! - 状态（§3）：提交 `goal_status` attachment 后才改目标（设定、清除、评估的判定都走这一条路），冷加载时
//!   从活动历史恢复；
//! - 判定（§4）：主循环报告评估结果，这里提交反馈与 `goal_status`。

use rust_cli_kernel_api::goal::{self, ActiveGoal, GoalStatus, GoalView};
use rust_cli_kernel_api::hooks::StopVerdict;
use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::message::{
    AttachmentMessage, ContentBlock, Message, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::store::StoreError;
use rust_cli_kernel_api::user_command::{self, LOCAL_COMMAND_SOURCE};
use rust_cli_prompts::texts::render;

use crate::actor::Actor;
use crate::slash::Inputs;

/// 条件的长度上限（按 UTF-16 码元计）。
const MAX_CONDITION: usize = 4000;
/// 清除目标的参数（不分大小写）。
const CLEAR: [&str; 6] = ["clear", "stop", "off", "reset", "none", "cancel"];

/// 输入是 `/goal` 命令时给出参数（trim 后）。
pub(crate) fn command(text: &str) -> Option<&str> {
    let rest = text.trim().strip_prefix("/goal")?;
    (rest.is_empty() || rest.starts_with(char::is_whitespace)).then(|| rest.trim())
}

/// 只中和 `forked-skill-launch` 标签。
fn neutralize(text: &str) -> String {
    text.replace("<forked-skill-launch>", "&lt;forked-skill-launch>")
        .replace("</forked-skill-launch>", "&lt;/forked-skill-launch>")
}

fn turns(count: u32) -> String {
    format!("{count} {}", if count == 1 { "turn" } else { "turns" })
}

/// 目标的状态：当前目标与本会话主线程累计的输出 token（费用账的近似，goal.md D5）。
#[derive(Debug, Default)]
pub(crate) struct Goals {
    active: Option<Active>,
    output_tokens: u64,
}

#[derive(Debug, Clone)]
struct Active {
    condition: String,
    iterations: u32,
    set_at_ms: u64,
    tokens_at_start: u64,
    last_reason: Option<String>,
}

fn status_of(message: &Message) -> Option<GoalStatus> {
    match message {
        Message::Attachment(attachment) if attachment.source == goal::STATUS_SOURCE => {
            serde_json::from_value(attachment.data.clone()).ok()
        }
        _ => None,
    }
}

impl Goals {
    /// 冷加载：最后一条 `goal_status` 既不是达成也不是失败时，它的条件重新生效，从 0 轮算起。
    pub(crate) fn restore(messages: &[Message], now_ms: u64) -> Self {
        let last = messages.iter().rev().find_map(status_of);
        let active = last
            .filter(|status| !status.met && !status.failed && !status.condition.is_empty())
            .map(|status| Active {
                condition: status.condition,
                iterations: 0,
                set_at_ms: now_ms,
                tokens_at_start: 0,
                last_reason: None,
            });
        Self {
            active,
            output_tokens: 0,
        }
    }

    pub(crate) fn condition(&self) -> Option<String> {
        self.active.as_ref().map(|active| active.condition.clone())
    }

    /// 一条消息提交之后：`goal_status` 改目标，主线程的 assistant 累计输出 token。
    pub(crate) fn observe(&mut self, message: &Message, now_ms: u64) {
        if let Message::Assistant(assistant) = message {
            self.output_tokens += assistant.usage.map_or(0, |usage| usage.output_tokens);
            return;
        }
        let Some(status) = status_of(message) else {
            return;
        };
        if status.sentinel && !status.met {
            self.active = Some(Active {
                condition: status.condition,
                iterations: 0,
                set_at_ms: now_ms,
                tokens_at_start: self.output_tokens,
                last_reason: None,
            });
            return;
        }
        let Some(active) = &mut self.active else {
            return;
        };
        if active.condition != status.condition {
            return;
        }
        if status.met || status.failed {
            self.active = None;
        } else {
            active.iterations += 1;
            active.last_reason = status.reason;
        }
    }

    /// `/goal` 面板（最近一次达成、不是标记的 `goal_status`）。
    pub(crate) fn view(&self, messages: &[Message], now_ms: u64) -> GoalView {
        let achieved = messages
            .iter()
            .rev()
            .filter_map(status_of)
            .find(|status| status.met && !status.sentinel);
        GoalView {
            active: self.active.as_ref().map(|active| ActiveGoal {
                condition: active.condition.clone(),
                iterations: active.iterations,
                set_at_ms: active.set_at_ms.min(now_ms),
                tokens: self.output_tokens - active.tokens_at_start,
                last_reason: active.last_reason.clone(),
            }),
            achieved,
        }
    }

    /// 只读的文字（非交互命令的空参数）。
    fn describe(&self) -> String {
        let Some(active) = &self.active else {
            return "No goal set. Usage: `/goal <condition>`".to_owned();
        };
        let progress = if active.iterations == 0 {
            "not yet evaluated".to_owned()
        } else {
            turns(active.iterations)
        };
        let last = active
            .last_reason
            .as_deref()
            .map(|reason| {
                let first = reason.trim().split('\n').next().unwrap_or_default();
                format!("\nLast check: {first}")
            })
            .unwrap_or_default();
        format!("Goal active: {} ({progress}){last}", active.condition)
    }

    /// 判定要提交的 `goal_status`（goal.md §3）；条件不是当前目标、或判定不改目标时为 None。
    fn checked(&self, condition: &str, verdict: &StopVerdict, now_ms: u64) -> Option<GoalStatus> {
        let active = self.active.as_ref().filter(|a| a.condition == condition)?;
        let finished = |met: bool, failed: bool, reason: &Option<String>| GoalStatus {
            met,
            sentinel: false,
            failed,
            condition: condition.to_owned(),
            reason: reason.clone(),
            iterations: Some(active.iterations + 1),
            duration_ms: Some(now_ms.saturating_sub(active.set_at_ms)),
            tokens: Some(self.output_tokens - active.tokens_at_start),
        };
        match verdict {
            StopVerdict::Met { reason } => Some(finished(true, false, reason)),
            StopVerdict::Impossible { reason } => Some(finished(false, true, reason)),
            StopVerdict::NotMet { reason, .. } => Some(GoalStatus {
                met: false,
                sentinel: false,
                failed: false,
                condition: condition.to_owned(),
                reason: reason.clone(),
                iterations: None,
                duration_ms: None,
                tokens: None,
            }),
            StopVerdict::Failed { .. } | StopVerdict::TimedOut | StopVerdict::Cancelled => None,
        }
    }
}

fn sentinel(met: bool, condition: &str) -> GoalStatus {
    GoalStatus {
        met,
        sentinel: true,
        failed: false,
        condition: condition.to_owned(),
        reason: None,
        iterations: None,
        duration_ms: None,
        tokens: None,
    }
}

impl Actor {
    fn status_message(&self, status: &GoalStatus) -> Message {
        Message::Attachment(AttachmentMessage {
            id: self.deps.ids.message(),
            source: goal::STATUS_SOURCE.to_owned(),
            text: String::new(),
            data: serde_json::to_value(status).unwrap_or_default(),
        })
    }

    fn meta(&self, source: &str, text: String) -> Message {
        Message::User(UserMessage {
            id: self.deps.ids.message(),
            content: vec![ContentBlock::Text { text }],
            origin: UserOrigin::Meta {
                source: source.to_owned(),
            },
        })
    }

    /// `/goal <args>` 写进对话的消息（goal.md §2.1）；第一条人类输入用回合预先生成的 `prompt` ID。
    pub(crate) fn goal_inputs(&self, args: &str, prompt: MessageId) -> Inputs {
        let input = Message::User(UserMessage {
            id: prompt,
            content: vec![ContentBlock::Text {
                text: user_command::command_input("goal", args),
            }],
            origin: UserOrigin::Human,
        });
        let stdout = |text: &str| {
            self.meta(
                LOCAL_COMMAND_SOURCE,
                format!(
                    "<local-command-stdout>{}</local-command-stdout>",
                    neutralize(text)
                ),
            )
        };
        let only = |text: String| Inputs {
            messages: vec![input.clone(), stdout(&text)],
            query: false,
            plain: false,
        };
        if args.is_empty() {
            return only(self.goal.describe());
        }
        if CLEAR.iter().any(|word| args.eq_ignore_ascii_case(word)) {
            return match self.goal.condition() {
                Some(condition) => Inputs {
                    messages: vec![
                        self.status_message(&sentinel(true, &condition)),
                        input.clone(),
                        stdout(&format!("Goal cleared: {condition}")),
                    ],
                    query: false,
                    plain: false,
                },
                None => only("No goal set".to_owned()),
            };
        }
        let length = args.encode_utf16().count();
        if length > MAX_CONDITION {
            return only(format!(
                "Goal condition is limited to {MAX_CONDITION} characters (got {length})"
            ));
        }
        Inputs {
            messages: vec![
                self.status_message(&sentinel(false, args)),
                input,
                stdout(&format!("Goal set: {args}")),
                self.meta(
                    goal::PROMPT_SOURCE,
                    render(rust_cli_prompts::goal::set_prompt(), &[("condition", args)]),
                ),
            ],
            query: true,
            plain: false,
        }
    }

    /// 提交后改目标（`record` 与 `record_all` 调用）。
    pub(crate) fn goal_committed(&mut self, message: &Message) {
        let now = self.deps.clock.now_ms();
        self.goal.observe(message, now);
    }

    /// 主循环报告的判定（`RunSink::goal_checked`）：提交反馈与 `goal_status`，返回已提交的消息。
    pub(crate) async fn goal_checked(
        &mut self,
        condition: String,
        verdict: StopVerdict,
    ) -> Result<Vec<Message>, StoreError> {
        let now = self.deps.clock.now_ms();
        let Some(status) = self.goal.checked(&condition, &verdict, now) else {
            return Ok(Vec::new());
        };
        let mut messages = Vec::new();
        if let StopVerdict::NotMet { feedback, .. } = verdict {
            messages.push(feedback);
        }
        messages.push(self.status_message(&status));
        self.record_all(messages.clone()).await?;
        Ok(messages)
    }

    pub(crate) fn goal_view(&self) -> GoalView {
        self.goal.view(&self.messages, self.deps.clock.now_ms())
    }
}

#[cfg(test)]
#[path = "goal_tests.rs"]
mod tests;
