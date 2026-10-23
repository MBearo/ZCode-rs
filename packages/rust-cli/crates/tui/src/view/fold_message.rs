//! 完整消息的折叠（docs/specs/rust-cli/tui.md §4.2）：`MessageDone` 与冷加载的消息；assistant 消息替换
//! 流式的响应段。
use rust_cli_kernel_api::goal;
use rust_cli_kernel_api::message::{ContentBlock, Message, SystemMessage, UserOrigin};
use rust_cli_kernel_api::{user_command, user_input};
use serde_json::Value;

use super::result_text;
use crate::view::{Body, Marker, PeerRow, Row, ToolRow, ToolStatus, Transcript};

/// 会话里同伴消息开回合的 meta 来源（session `peers.rs` 的 `PEER_SOURCE`）。
const PEER_SOURCE: &str = "cross-session";
/// 同伴消息给模型的包装的第一行（工具边界与空闲两种，`prompts/peers/wrap_*.txt`）。
const PEER_HEADS: [&str; 2] = [
    "Another ZCode session sent a message while you were working:\n",
    "Another ZCode session sent a message:\n",
];
/// 回执的开头（`prompts/peers/non_user_prefix.txt` 与 `receipt_*.txt`）。
const RECEIPT_HEAD: &str =
    "[MESSAGE FROM NON-USER SOURCE - NOT USER INPUT]\n[Cross-session delivery notice] ";

/// 并入的文字 → 跨会话的一行；不是同伴消息或回执时为 None。
fn peer_row(text: &str) -> Option<PeerRow> {
    if let Some(rest) = text.strip_prefix(RECEIPT_HEAD) {
        let status = [
            ("held", " held by that session"),
            ("denied", " denied by the recipient user"),
            ("expired", " not approved before expiry"),
            ("delivered", " approved and released"),
            ("refused", " refused"),
        ]
        .into_iter()
        .find(|(_, words)| rest.contains(words))
        .map(|(status, _)| status.to_owned())?;
        return Some(PeerRow::Receipt { status });
    }
    let body = PEER_HEADS.iter().find_map(|head| text.strip_prefix(head))?;
    let end = body.find("\n</cross-session-message>")? + "\n</cross-session-message>".len();
    let envelope = rust_cli_kernel_api::peers::parse_envelope(&body[..end])?;
    // 名字：`from-name`，没有时是 socket 文件名，再没有时是 `peer`。
    let name = envelope
        .from_name
        .clone()
        .or_else(|| {
            let from = envelope.from.as_deref()?;
            let socket = rust_cli_kernel_api::peers::PeerAddress::parse(from)?.socket();
            let file = socket
                .rsplit(['/', '\\'])
                .next()?
                .trim_end_matches(".sock")
                .to_owned();
            (!file.is_empty()).then_some(file)
        })
        .unwrap_or_else(|| "peer".to_owned());
    Some(PeerRow::Message {
        name,
        body: envelope.body,
    })
}

impl Transcript {
    pub(super) fn message(&mut self, message: Message, now_ms: u64) {
        match message {
            Message::User(user) => match user.origin {
                UserOrigin::Human => {
                    let text = user
                        .content
                        .iter()
                        .filter_map(|block| match block {
                            ContentBlock::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    // bash 模式的输入与输出（tui-input.md §2.1）。
                    if let Some(command) = user_input::display(&text) {
                        let command = command.trim_start_matches("! ").to_owned();
                        self.push(Body::Shell { command }, now_ms);
                        return;
                    }
                    if let Some((stdout, stderr)) = user_input::output_parts(&text) {
                        let (stdout, stderr) = (unescape(&stdout), unescape(&stderr));
                        self.push(Body::ShellOutput { stdout, stderr }, now_ms);
                        return;
                    }
                    // 用户调用 skill 的输入是命令标签：显示成用户输入的样子（ext.md §2.7）。
                    let text = user_command::display(&text).unwrap_or(text);
                    self.push(Body::User { text }, now_ms);
                }
                UserOrigin::ToolResult { .. } => {
                    for block in user.content {
                        if let ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                        } = block
                        {
                            self.update_tool(&tool_use_id, |tool| {
                                if tool.status.is_live() || tool.output.is_empty() {
                                    tool.output = result_text(&content);
                                    tool.status = if is_error {
                                        ToolStatus::Error
                                    } else {
                                        ToolStatus::Success
                                    };
                                }
                            });
                        }
                    }
                }
                UserOrigin::Meta { source } if source == user_command::LOCAL_COMMAND_SOURCE => {
                    let text = user
                        .content
                        .iter()
                        .find_map(|block| match block {
                            ContentBlock::Text { text } => Some(local_output(text)),
                            _ => None,
                        })
                        .unwrap_or_default();
                    self.push(Body::Output(text), now_ms);
                }
                // 同伴消息开的回合（cross-session.md §2.6）：显示成 `@ 名字 ›`。
                UserOrigin::Meta { source } if source == PEER_SOURCE => {
                    let text: String = user
                        .content
                        .iter()
                        .filter_map(|block| match block {
                            ContentBlock::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect();
                    if let Some(row) = peer_row(&text) {
                        self.push(Body::Peer(Box::new(row)), now_ms);
                    }
                }
                UserOrigin::Meta { .. } | UserOrigin::CompactSummary => {}
            },
            Message::Assistant(assistant) => {
                self.add_usage(&assistant.model, assistant.usage);
                self.replace_segment(assistant.content, now_ms);
            }
            // 工具边界并入的同伴消息与回执（cross-session.md §2.6）。
            Message::Attachment(attachment)
                if attachment.source == "queued_command"
                    && peer_row(&attachment.text).is_some() =>
            {
                if let Some(row) = peer_row(&attachment.text) {
                    self.push(Body::Peer(Box::new(row)), now_ms);
                }
            }
            // 插话显示成用户行；工具边界并入的后台通知（`task-notification`，L18）与开回合的通知 meta
            // 消息一样不显示。
            Message::Attachment(attachment)
                if attachment.source == "queued_command"
                    && attachment.data["commandMode"] != "task-notification" =>
            {
                let text = attachment.data["prompt"].as_str().unwrap_or_default();
                self.push(Body::User { text: text.into() }, now_ms);
            }
            Message::Attachment(attachment) if attachment.source == goal::STATUS_SOURCE => {
                let status = serde_json::from_value::<goal::GoalStatus>(attachment.data).ok();
                if let Some(status) = status.filter(|status| !status.sentinel) {
                    self.push(Body::Goal(Box::new(status)), now_ms);
                }
            }
            Message::Attachment(_) => {}
            Message::System(SystemMessage::CompactBoundary(_)) => {
                self.push(Body::Marker(Marker::Compacted), now_ms);
            }
            // 中断由回合行显示。
            Message::System(SystemMessage::Interrupted { .. }) => {}
        }
    }

    /// 用完整的 assistant 消息替换响应段：流式行换成定稿的行，已有的工具行按块的顺序保留。
    fn replace_segment(&mut self, content: Vec<ContentBlock>, now_ms: u64) {
        let start = self
            .segment
            .take()
            .map_or(self.rows.len(), |segment| segment.start);
        let mut old: Vec<Row> = self.rows.drain(start.min(self.rows.len())..).collect();
        let mut reasoning_starts = old
            .iter()
            .filter(|row| matches!(row.body, Body::Reasoning { .. }))
            .map(|row| row.created_ms)
            .collect::<Vec<_>>()
            .into_iter();
        for block in content {
            match block {
                ContentBlock::Text { text } if !text.trim().is_empty() => {
                    self.push(
                        Body::Agent {
                            text,
                            streaming: false,
                        },
                        now_ms,
                    );
                }
                ContentBlock::Thinking { thinking, .. } => {
                    let started = reasoning_starts.next().unwrap_or(now_ms);
                    self.push(
                        Body::Reasoning {
                            text: thinking,
                            streaming: false,
                            duration_ms: Some(now_ms.saturating_sub(started)),
                        },
                        started,
                    );
                }
                ContentBlock::ToolUse { id, name, input } => {
                    let existing = old
                        .iter()
                        .position(|row| row.tool().is_some_and(|tool| tool.id == id));
                    match existing {
                        Some(at) => self.rows.push(old.remove(at)),
                        None => {
                            self.push(
                                Body::Tool(Box::new(ToolRow {
                                    id,
                                    name,
                                    input,
                                    status: ToolStatus::Pending,
                                    output: String::new(),
                                    preview: String::new(),
                                    display: Value::Null,
                                    activity: None,
                                })),
                                now_ms,
                            );
                        }
                    }
                }
                _ => {}
            }
        }
        // 段里不属于这条消息的工具行（不应出现）：保留在后面。
        self.rows
            .extend(old.into_iter().filter(|row| row.tool().is_some()));
    }
}

/// 本地命令的输出：去掉 `<local-command-stdout>` / `<local-command-stderr>` 包装并还原转义。
fn local_output(text: &str) -> String {
    let inner = ["local-command-stdout", "local-command-stderr"]
        .iter()
        .find_map(|tag| {
            text.strip_prefix(&format!("<{tag}>"))?
                .strip_suffix(&format!("</{tag}>"))
        });
    match inner {
        Some(inner) => inner
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&"),
        None => text.to_owned(),
    }
}

/// [`user_input::escape_xml`] 的反向（显示用）。
fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}
