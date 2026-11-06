//! 完整消息的折叠（docs/specs/rust-cli/tui.md §4.2）：`MessageDone` 与冷加载的消息；assistant 消息替换
//! 流式的响应段。
use rust_cli_kernel_api::goal;
use rust_cli_kernel_api::hooks::queued_command;
use rust_cli_kernel_api::message::{ContentBlock, Message, SystemMessage, UserOrigin};
use rust_cli_kernel_api::{user_command, user_input};
use rust_cli_tui_kit::app::widgets::entries::Compaction;

use crate::view::{Body, Compact, PeerRow, Row, ToolRow, ToolStatus, Transcript};
use rust_cli_kernel_api::message::text_of;

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

/// `note` 里 `<tag>` 与 `</tag>` 之间的文字。
fn tagged<'a>(note: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let start = note.find(&open)? + open.len();
    let len = note[start..].find(&format!("</{tag}>"))?;
    Some(&note[start..start + len])
}

/// 后台工作的结果通知（`<task-notification>`）：一段文字可以并入几条。
fn notifications(text: &str) -> impl Iterator<Item = &str> {
    text.split("<task-notification>").skip(1)
}

impl Transcript {
    /// 记下 `text` 里收到了结果的后台工作；第一次收到的后台子代理的结果记进 `returned`（通知行的标题）。
    fn report(&mut self, text: &str) {
        for note in notifications(text) {
            let Some(id) = tagged(note, "task-id") else {
                continue;
            };
            if !self.reported.insert(id.to_owned()) {
                continue;
            }
            if let Some(title) = self.subagent_title(note) {
                self.returned.push(title);
            }
        }
    }

    /// 结果通知是后台子代理的时它的标题（Puffin `subagent`：Agent 调用的 `description`，没有时 `subagent_type`）；
    /// 调用不在视图里（压缩掉了、续跑的通知没有 `<tool-use-id>`）时取摘要 `Agent "描述" …` 里的描述。不是子代理时 None。
    fn subagent_title(&self, note: &str) -> Option<String> {
        let call = tagged(note, "tool-use-id").and_then(|id| {
            self.rows.iter().rev().find_map(|row| match &row.body {
                Body::Tool(tool) if tool.id.as_str() == id && tool.name == "Agent" => Some(tool),
                _ => None,
            })
        });
        if let Some(tool) = call {
            let input = &tool.input;
            return Some(
                input["description"]
                    .as_str()
                    .or_else(|| input["subagent_type"].as_str())
                    .unwrap_or_default()
                    .to_owned(),
            );
        }
        let summary = tagged(note, "summary")?.strip_prefix("Agent \"")?;
        let end = summary.rfind('"')?;
        Some(unescape(&summary[..end]))
    }
}

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
                // 中断后补的那条消息不是你说的话（Puffin `user_message`）：不成行。以前画成一块你的消息。
                UserOrigin::Human if user_input::is_interrupt(&user) => {}
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
                        // 接替 ⏎ 时画的那个块（Puffin `shell_input` 接上 `shell_started` 的块）：同一条命令，块留在原处。
                        self.drop_shell(&command);
                        self.shell_open = true;
                        self.push(Body::Shell { command }, now_ms);
                        return;
                    }
                    if let Some((stdout, stderr)) = user_input::output_parts(&text) {
                        let (stdout, stderr) = (unescape(&stdout), unescape(&stderr));
                        self.shell_open = false;
                        self.push(Body::ShellOutput { stdout, stderr }, now_ms);
                        return;
                    }
                    // 用户调用 skill 的输入是命令标签：显示成用户输入的样子（ext.md §2.7）。
                    let text = user_command::display(&text).unwrap_or(text);
                    if self.turn_user.is_none() {
                        self.turn_user = Some(text.clone());
                    }
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
                                    tool.output = text_of(&content);
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
                // 内核为后台工作的结果开的回合（不显示）：记下哪些工作的结果主会话收到了（后台子代理的坞，§1.8）。
                UserOrigin::Meta { .. } => {
                    for block in &user.content {
                        if let ContentBlock::Text { text } = block {
                            self.report(text);
                        }
                    }
                }
                UserOrigin::CompactSummary => {}
            },
            // 续写时补的 assistant（session-lifecycle.md §6.3.1）：不显示。
            Message::Assistant(assistant) if assistant.is_resume_sentinel() => {}
            Message::Assistant(assistant) => {
                // 一次模型调用结束：现场速度用它量到的输出校准（Puffin fold 的 `usage.live.last_call`）。
                if let Some(usage) = &assistant.usage {
                    self.last_call = Some((assistant.id.to_string(), usage.output_tokens));
                }
                self.add_usage(&assistant.model, assistant.usage);
                self.replace_segment(assistant.id.to_string(), assistant.content, now_ms);
            }
            // 工具边界并入的同伴消息与回执（cross-session.md §2.6）。
            Message::Attachment(attachment)
                if attachment.source == queued_command::SOURCE
                    && peer_row(&attachment.text).is_some() =>
            {
                if let Some(row) = peer_row(&attachment.text) {
                    self.push(Body::Peer(Box::new(row)), now_ms);
                }
            }
            // 插话显示成用户行；工具边界并入的后台通知（`task-notification`，L18）与开回合的通知 meta
            // 消息一样不显示。
            Message::Attachment(attachment)
                if attachment.source == queued_command::SOURCE
                    && attachment.data[queued_command::MODE_KEY]
                        != queued_command::TASK_NOTIFICATION =>
            {
                let text = attachment.data[queued_command::PROMPT_KEY]
                    .as_str()
                    .unwrap_or_default();
                self.push(Body::User { text: text.into() }, now_ms);
            }
            Message::Attachment(attachment) if attachment.source == goal::STATUS_SOURCE => {
                let status = serde_json::from_value::<goal::GoalStatus>(attachment.data).ok();
                if let Some(status) = status.filter(|status| !status.sentinel) {
                    self.push(Body::Goal(Box::new(status)), now_ms);
                }
            }
            // 工具边界并入的后台通知（不显示）：同上记下收到了结果的工作。
            Message::Attachment(attachment)
                if attachment.source == "queued_command"
                    && attachment.data["commandMode"] == "task-notification" =>
            {
                let prompt = attachment.data["prompt"].as_str().unwrap_or_default();
                self.report(prompt);
                self.report(&attachment.text);
            }
            Message::Attachment(_) => {}
            // 压缩的边界：在跑的压缩记下压缩前的 token；冷加载（没有在跑的压缩）是一次成功了的压缩（Puffin `compact_boundary`）。
            Message::System(SystemMessage::CompactBoundary(boundary)) => match self.compact {
                Some((id, _)) => {
                    self.edit_compact(id, |compact| compact.before = Some(boundary.pre_tokens));
                }
                None => {
                    self.push(
                        Body::Compact(Compact {
                            status: Compaction::Success,
                            before: Some(boundary.pre_tokens),
                            after: None,
                        }),
                        now_ms,
                    );
                }
            },
            // 中断由回合行显示。
            Message::System(SystemMessage::Interrupted { .. }) => {}
        }
    }

    /// 用完整的 assistant 消息替换响应段：流式行换成定稿的行，已有的工具行按块的顺序保留。
    fn replace_segment(&mut self, message: String, content: Vec<ContentBlock>, now_ms: u64) {
        let segment = self.segment.take();
        // 这条消息的调用共有的 `response`：流式的段的序号，没有流时是消息 id（Puffin `replace_segment` 的 `key`）。
        let key = segment
            .as_ref()
            .map(|segment| segment.key.clone())
            .filter(|key| !key.is_empty())
            .unwrap_or(message);
        let start = segment
            .as_ref()
            .map_or(self.rows.len(), |segment| segment.start)
            .min(self.rows.len());
        let blocks = segment.map(|segment| segment.blocks).unwrap_or_default();
        let mut old: Vec<Row> = self.rows.drain(start..).collect();
        // 流过的块定稿时留着它的行 id（Puffin 的条目 id 不变）：播放头与动效按 id 接着走，不从头再打一遍。以前定稿的行是新行。
        let streamed: Vec<(usize, u64, u64, u64)> = blocks
            .iter()
            .filter_map(|(index, at)| {
                let row = old.get(at.checked_sub(start)?)?;
                Some((*index, row.id, row.version, row.created_ms))
            })
            .collect();
        let of = |index: usize| streamed.iter().find(|(i, ..)| *i == index).copied();
        for (index, block) in content.into_iter().enumerate() {
            match block {
                ContentBlock::Text { text } if !text.trim().is_empty() => {
                    let body = Body::Agent {
                        text,
                        streaming: false,
                    };
                    match of(index) {
                        Some((_, id, version, created_ms)) => self.rows.push(Row {
                            id,
                            version: version + 1,
                            created_ms,
                            body,
                        }),
                        None => {
                            self.push(body, now_ms);
                        }
                    }
                }
                // 空白的思考不成行；没有流过的思考不知道想了多久（Puffin `replace_segment`）。
                ContentBlock::Thinking { thinking, .. } if !thinking.trim().is_empty() => {
                    match of(index) {
                        Some((_, id, version, created_ms)) => self.rows.push(Row {
                            id,
                            version: version + 1,
                            created_ms,
                            body: Body::Reasoning {
                                text: thinking,
                                streaming: false,
                                duration_ms: Some(now_ms.saturating_sub(created_ms)),
                            },
                        }),
                        None => {
                            self.push(
                                Body::Reasoning {
                                    text: thinking,
                                    streaming: false,
                                    duration_ms: None,
                                },
                                now_ms,
                            );
                        }
                    }
                }
                ContentBlock::ToolUse { id, name, input } => {
                    let existing = old
                        .iter()
                        .position(|row| row.tool().is_some_and(|tool| tool.id == id));
                    match existing {
                        Some(at) => self.rows.push(old.remove(at)),
                        None => {
                            let mut tool = ToolRow::new(id, name, input, ToolStatus::Pending);
                            tool.response = key.clone();
                            self.push(Body::Tool(Box::new(tool)), now_ms);
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
