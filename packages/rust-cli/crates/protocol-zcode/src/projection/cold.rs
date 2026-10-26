//! 冷投影：从会话视图（已提交的消息）重建行。没有事件的时间，行的时间取会话的创建时间；回合以真人输入为界。

use rust_cli_kernel_api::message::{ContentBlock, Message, SystemMessage, UserOrigin};
use rust_cli_kernel_api::session_api::{SequencedEvent, SessionView};
use rust_cli_kernel_api::user_input::INTERRUPT_TEXTS;

use super::{Projection, state::Phase, tool_key};

/// 从视图的消息建行；阶段按最后一个回合与 `running` 定。
pub(super) fn rows(projection: &mut Projection, view: &SessionView) {
    let at = view.meta.created_at_ms;
    let mut out = Vec::new();
    let mut turns = 0u64;
    let mut interrupted = false;
    for message in &view.messages {
        match message {
            Message::User(user)
                if user.origin == UserOrigin::Human
                    && !INTERRUPT_TEXTS.contains(&super::text_of(&user.content).as_str()) =>
            {
                if projection.turn.is_some() {
                    projection.close_cold_turn(interrupted, at, &mut out);
                }
                interrupted = false;
                turns += 1;
                let turn = format!("{}:turn:{turns}", user.id.as_str());
                projection.start_turn(&turn, None, at, 0, &mut out);
                projection.message(message, at, 0, &mut out);
            }
            Message::User(user) => {
                if INTERRUPT_TEXTS.contains(&super::text_of(&user.content).as_str()) {
                    interrupted = true;
                }
                // 工具结果：给对应的工具行结果。
                for block in &user.content {
                    if let ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } = block
                    {
                        let output = rust_cli_kernel_api::tool::ToolOutput {
                            content: content.clone(),
                            is_error: *is_error,
                            display: serde_json::Value::Null,
                        };
                        let event = SequencedEvent {
                            seq: 0,
                            event: rust_cli_kernel_api::session_api::KernelEvent::ToolDone {
                                tool_use: tool_use_id.clone(),
                                output,
                            },
                            revision: projection.revision,
                        };
                        if projection
                            .index
                            .contains_key(&tool_key(tool_use_id.as_str()))
                        {
                            projection.apply(&event, at);
                        }
                    }
                }
            }
            Message::Assistant(_) => projection.message(message, at, 0, &mut out),
            Message::System(SystemMessage::Interrupted { .. }) => interrupted = true,
            _ => {}
        }
    }
    if projection.turn.is_some() {
        if view.running {
            projection.phase = Phase::Running;
        } else {
            projection.close_cold_turn(interrupted, at, &mut out);
        }
    }
}

impl Projection {
    fn close_cold_turn(&mut self, interrupted: bool, at: u64, out: &mut Vec<serde_json::Value>) {
        let Some(turn) = self.turn.as_ref().map(|turn| turn.id.clone()) else {
            return;
        };
        let outcome = if interrupted {
            rust_cli_kernel_api::session_api::TurnOutcome::Interrupted
        } else {
            rust_cli_kernel_api::session_api::TurnOutcome::Success
        };
        self.finish_turn(&turn, &outcome, None, at, out);
    }
}
