//! 对话中间的 `role: system` 消息降级（docs/specs/rust-cli/model.md §10）。
//!
//! 模型不接受这种消息时（Provider 配置 `supportsMidConversationSystem` 为假），把它们改成 user 消息里的
//! `<system-reminder>` 文本，再与相邻的 user 消息合并——这是关掉 mid-conversation-system beta
//! 时的形状。合并时工具结果保持在 user 消息最前（Anthropic 要求 tool_result 紧跟发出调用的 assistant）；
//! 前一条 user 消息以工具结果结尾时提醒并进工具结果（docs/specs/rust-cli/context.md §3 第 9 条）。
//! 消息级的 `output_config`（`ApiMessage::output_config`）只能写在 system 消息上，降级后一并去掉；
//! 请求级的 effort 不受影响。

use std::collections::HashMap;

use rust_cli_kernel_api::ids::ToolUseId;
use rust_cli_kernel_api::message::ContentBlock;
use rust_cli_kernel_api::model::{ApiContent, ApiMessage, ApiRole, ModelRequest};
use rust_cli_kernel_api::reminder_fold::{array_result, fold_into_tool_result};

pub(crate) fn fold(request: &mut ModelRequest) {
    if !request
        .messages
        .iter()
        .any(|message| message.role == ApiRole::System)
    {
        return;
    }
    let messages = std::mem::take(&mut request.messages);
    let mut out: Vec<ApiMessage> = Vec::with_capacity(messages.len());
    let mut tool_names: HashMap<ToolUseId, String> = HashMap::new();
    for mut message in messages {
        message.output_config = None;
        let reminder = message.role == ApiRole::System;
        if reminder {
            message.role = ApiRole::User;
            for content in &mut message.content {
                if let ContentBlock::Text { text } = &mut content.block {
                    *text = rust_cli_base::reminder::wrap(text);
                }
            }
        }
        for content in &message.content {
            if let ContentBlock::ToolUse { id, name, .. } = &content.block {
                tool_names.insert(id.clone(), name.clone());
            }
        }
        match out.last_mut() {
            Some(last) if last.role == ApiRole::User && reminder => {
                for content in message.content {
                    fold_reminder(last, content, &tool_names);
                }
            }
            Some(last) if last.role == ApiRole::User && message.role == ApiRole::User => {
                merge(last, message.content);
            }
            _ => out.push(message),
        }
    }
    request.messages = out;
}

/// 降级后的一段提醒接到前一条 user 消息：末尾是工具结果时并进它，缓存断点随之移到工具结果上
/// （kernel-api `reminder_fold`，context.md §3 第 9 条）；否则接在后面。
///
/// 缺陷修复（R4 F7）：原来总是另起文本块，断点落在文本块上；关掉 system turn 时应并进工具结果。
fn fold_reminder(
    last: &mut ApiMessage,
    content: ApiContent,
    tool_names: &HashMap<ToolUseId, String>,
) {
    let folded = match &content.block {
        ContentBlock::Text { text } => {
            fold_into_tool_result(&mut last.content, text, |id, is_error| {
                tool_names
                    .get(id)
                    .is_some_and(|name| array_result(name, is_error))
            })
        }
        _ => false,
    };
    if !folded {
        merge(last, vec![content]);
        return;
    }
    if let (Some(cache), Some(result)) = (content.cache_control, last.content.last_mut()) {
        result.cache_control = Some(cache);
    }
}

/// 把 `next` 并进前一条 user 消息：`next` 里的工具结果排到最前，其余内容接在原有内容之后。
fn merge(last: &mut ApiMessage, next: Vec<ApiContent>) {
    let (results, rest): (Vec<_>, Vec<_>) = next
        .into_iter()
        .partition(|content| matches!(content.block, ContentBlock::ToolResult { .. }));
    if !results.is_empty() {
        let previous = std::mem::replace(&mut last.content, results);
        last.content.extend(previous);
    }
    last.content.extend(rest);
}

#[cfg(test)]
mod tests {
    use rust_cli_kernel_api::ids::ToolUseId;
    use rust_cli_kernel_api::model::{QuerySource, RequestOptions, TurnOutputConfig};

    use super::*;

    fn text(role: ApiRole, body: &str) -> ApiMessage {
        ApiMessage {
            role,
            content: vec![ApiContent {
                block: ContentBlock::Text {
                    text: body.to_owned(),
                },
                cache_control: None,
            }],
            output_config: None,
        }
    }

    fn request(messages: Vec<ApiMessage>) -> ModelRequest {
        ModelRequest {
            selection: rust_cli_kernel_api::model::ModelSelection {
                provider: "p".to_owned(),
                model: "m".to_owned(),
                reasoning: None,
                connection: None,
            },
            system: Vec::new(),
            messages,
            tools: Vec::new(),
            tool_choice: None,
            max_tokens: 1,
            thinking: None,
            temperature: None,
            betas: Vec::new(),
            source: QuerySource::MainThread,
            options: RequestOptions::default(),
        }
    }

    fn texts(message: &ApiMessage) -> Vec<String> {
        message
            .content
            .iter()
            .map(|content| match &content.block {
                ContentBlock::Text { text } => text.clone(),
                ContentBlock::ToolResult { .. } => "<result>".to_owned(),
                other => format!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn system_messages_become_reminders_in_the_user_turn() {
        let mut request = request(vec![
            text(ApiRole::User, "hi"),
            ApiMessage {
                output_config: Some(TurnOutputConfig {
                    effort: Some("medium".to_owned()),
                }),
                ..text(ApiRole::System, "env")
            },
        ]);
        fold(&mut request);
        assert_eq!(request.messages.len(), 1);
        assert_eq!(
            texts(&request.messages[0]),
            ["hi", "<system-reminder>\nenv\n</system-reminder>"]
        );
        assert!(request.messages[0].output_config.is_none());
    }

    #[test]
    fn tool_results_stay_first_when_a_reminder_precedes_them() {
        let result = ApiMessage {
            role: ApiRole::User,
            content: vec![ApiContent {
                block: ContentBlock::ToolResult {
                    tool_use_id: ToolUseId::new("t1"),
                    content: Vec::new(),
                    is_error: false,
                },
                cache_control: None,
            }],
            output_config: None,
        };
        let mut request = request(vec![
            text(ApiRole::Assistant, "calling"),
            text(ApiRole::System, "note"),
            result,
        ]);
        fold(&mut request);
        assert_eq!(request.messages.len(), 2);
        assert_eq!(
            texts(&request.messages[1]),
            ["<result>", "<system-reminder>\nnote\n</system-reminder>"]
        );
    }

    /// 降级后的提醒遇到末尾的工具结果时并进它，缓存断点随之移到工具结果上（R4 F7）。
    #[test]
    fn a_reminder_after_a_tool_result_joins_it_and_takes_the_breakpoint() {
        let tool_use = ToolUseId::new("t1");
        let call = ApiMessage {
            role: ApiRole::Assistant,
            content: vec![ApiContent {
                block: ContentBlock::ToolUse {
                    id: tool_use.clone(),
                    name: "Read".to_owned(),
                    input: serde_json::Value::Null,
                },
                cache_control: None,
            }],
            output_config: None,
        };
        let result = ApiMessage {
            role: ApiRole::User,
            content: vec![ApiContent {
                block: ContentBlock::ToolResult {
                    tool_use_id: tool_use,
                    content: vec![rust_cli_kernel_api::message::ToolResultContent::Text {
                        text: "ok\n".to_owned(),
                    }],
                    is_error: false,
                },
                cache_control: None,
            }],
            output_config: None,
        };
        let mut note = text(ApiRole::System, "note");
        note.content[0].cache_control = Some(rust_cli_kernel_api::model::CacheControl {
            kind: "ephemeral".to_owned(),
            ttl: None,
            scope: None,
        });
        let mut request = request(vec![text(ApiRole::User, "hi"), call, result, note]);
        fold(&mut request);
        assert_eq!(request.messages.len(), 3);
        let last = &request.messages[2];
        assert_eq!(last.content.len(), 1);
        assert!(last.content[0].cache_control.is_some());
        let ContentBlock::ToolResult { content, .. } = &last.content[0].block else {
            panic!("not a tool result");
        };
        assert_eq!(
            content,
            &[rust_cli_kernel_api::message::ToolResultContent::Text {
                text: "ok\n\n<system-reminder>\nnote\n</system-reminder>".to_owned()
            }]
        );
    }

    #[test]
    fn requests_without_system_messages_are_untouched() {
        let mut request = request(vec![text(ApiRole::User, "hi")]);
        let before = request.clone();
        fold(&mut request);
        assert_eq!(request, before);
    }
}
