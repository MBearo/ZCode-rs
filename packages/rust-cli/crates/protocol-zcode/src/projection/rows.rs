//! 行的增量（[`Projection`] 的一部分，从 mod.rs 拆出：800 行上限）：流式正文与思考、落定的消息、工具调用行与权限询问。

use rust_cli_kernel_api::message::{ContentBlock, Message, UserOrigin};
use rust_cli_kernel_api::model::BlockDelta;
use rust_cli_kernel_api::orchestration::InteractionRequest;
use rust_cli_kernel_api::user_input::INTERRUPT_TEXTS;
use serde_json::{Value, json};

use super::{Projection, block_key, state, text_of, tool_key};

impl Projection {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn stream(
        &mut self,
        turn: &str,
        response: u32,
        index: usize,
        delta: &BlockDelta,
        now: u64,
        seq: u64,
        out: &mut Vec<Value>,
    ) {
        let (kind, text) = match delta {
            BlockDelta::Text(text) => ("assistantText", text),
            BlockDelta::Thinking(text) => ("reasoning", text),
            BlockDelta::Signature(_) | BlockDelta::InputJson(_) => return,
        };
        if let Some(current) = &mut self.turn {
            current.response = Some(response);
        }
        let key = block_key(kind, turn, response, index);
        if !self.index.contains_key(&key) {
            let entity = format!("{turn}:{response}:{index}");
            let mut row = self.base(turn, &entity, kind, now, seq);
            row.insert("text".into(), json!(""));
            row.insert("state".into(), json!("streaming"));
            self.append(key.clone(), row, out);
        }
        let Some(&at) = self.index.get(&key) else {
            return;
        };
        let row = &mut self.rows[at];
        let joined = format!("{}{text}", row["text"].as_str().unwrap_or_default());
        row.insert("text".into(), json!(joined));
        let row_id = row["rowId"].clone();
        out.push(json!({"op": "row.delta", "rowId": row_id, "path": "text", "append": text}));
    }

    pub(super) fn message(&mut self, message: &Message, now: u64, seq: u64, out: &mut Vec<Value>) {
        match message {
            // 用户中断后补的那条消息只给模型看（Node 不显示）。
            Message::User(user)
                if user.origin == UserOrigin::Human
                    && !INTERRUPT_TEXTS.contains(&text_of(&user.content).as_str()) =>
            {
                let Some(turn) = self.turn.clone() else {
                    return;
                };
                let key = format!("user:{}", user.id.as_str());
                if self.index.contains_key(&key) {
                    return;
                }
                let mut row = self.base(&turn.id, user.id.as_str(), "userInput", now, seq);
                row.insert("text".into(), json!(text_of(&user.content)));
                row.insert("origin".into(), json!("realUser"));
                if let Some(command) = &turn.command {
                    row.insert("sourceCommandId".into(), json!(command));
                    row.insert("rootSourceCommandId".into(), json!(command));
                    if let Some(meta) = self.inputs.get(command) {
                        if let Some(origin) = &meta.origin {
                            row.insert("inputOrigin".into(), json!(origin));
                        }
                        row.insert("clientId".into(), json!(meta.client));
                    }
                }
                self.append(key, row, out);
            }
            Message::Assistant(assistant) => {
                let Some(turn) = self.turn.clone() else {
                    return;
                };
                let response = turn.response.unwrap_or(u32::MAX);
                for (index, block) in assistant.content.iter().enumerate() {
                    match block {
                        ContentBlock::Text { text } => {
                            self.settle_block(
                                "assistantText",
                                &turn.id,
                                response,
                                index,
                                text,
                                assistant,
                                now,
                                seq,
                                out,
                            );
                        }
                        ContentBlock::Thinking { thinking, .. } => {
                            self.settle_block(
                                "reasoning",
                                &turn.id,
                                response,
                                index,
                                thinking,
                                assistant,
                                now,
                                seq,
                                out,
                            );
                        }
                        ContentBlock::ToolUse { id, name, input } => {
                            if !self.index.contains_key(&tool_key(id.as_str())) {
                                self.tool_row(id.as_str(), name, input, now, seq, out);
                            }
                            let message_id = assistant.id.as_str().to_owned();
                            self.upsert(
                                &tool_key(id.as_str()),
                                |row| {
                                    row.insert("assistantResponseId".into(), json!(message_id));
                                },
                                out,
                            );
                        }
                        _ => {}
                    }
                }
                if let Some(current) = &mut self.turn {
                    current.response = None;
                }
            }
            _ => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn settle_block(
        &mut self,
        kind: &str,
        turn: &str,
        response: u32,
        index: usize,
        text: &str,
        assistant: &rust_cli_kernel_api::message::AssistantMessage,
        now: u64,
        seq: u64,
        out: &mut Vec<Value>,
    ) {
        if text.is_empty() {
            return;
        }
        let key = block_key(kind, turn, response, index);
        if !self.index.contains_key(&key) {
            let mut row = self.base(
                turn,
                &format!("{}:{index}", assistant.id.as_str()),
                kind,
                now,
                seq,
            );
            row.insert("text".into(), json!(text));
            row.insert("state".into(), json!("streaming"));
            self.append(key.clone(), row, out);
        }
        let (message_id, model) = (assistant.id.as_str().to_owned(), assistant.model.clone());
        let assistant_text = kind == "assistantText";
        self.upsert(
            &key,
            |row| {
                row.insert("text".into(), json!(text));
                row.insert("state".into(), json!("complete"));
                row.insert("assistantResponseId".into(), json!(message_id));
                if assistant_text {
                    row.insert("model".into(), json!(model));
                }
            },
            out,
        );
    }

    pub(super) fn tool_row(
        &mut self,
        tool_use: &str,
        name: &str,
        input: &Value,
        now: u64,
        seq: u64,
        out: &mut Vec<Value>,
    ) {
        let turn = self
            .turn
            .as_ref()
            .map_or_else(|| self.session.clone(), |turn| turn.id.clone());
        let mut row = self.base(&turn, tool_use, "toolCall", now, seq);
        row.insert("toolCallId".into(), json!(tool_use));
        row.insert("toolName".into(), json!(name));
        row.insert("status".into(), json!("running"));
        row.insert("inputText".into(), json!(input.to_string()));
        row.insert("input".into(), input.clone());
        self.append(tool_key(tool_use), row, out);
    }

    /// 登记一个询问（只有权限询问进 pendingInteractions，P1）。返回它所属的工具调用。
    pub(super) fn ask(
        &mut self,
        interaction: &str,
        request: &InteractionRequest,
        now: u64,
    ) -> Option<String> {
        let InteractionRequest::Permission {
            tool_use,
            tool,
            input,
            message,
            ..
        } = request
        else {
            return None;
        };
        let anchor = self.row_id(&tool_key(tool_use.as_str()));
        self.pending.push(json!({
            "interactionId": interaction,
            "kind": "permission",
            "anchorRowId": anchor,
            "createdAt": now,
            "payload": {
                "kind": "permission",
                "toolCallId": tool_use.as_str(),
                "toolName": tool,
                "summary": message,
                "detail": input,
                "freeText": true,
                "fullAccessOption": state::full_access_option(),
                "options": state::permission_options(),
            },
        }));
        Some(tool_use.as_str().to_owned())
    }
}
