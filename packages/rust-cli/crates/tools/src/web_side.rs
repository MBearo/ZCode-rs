//! WebFetch 的旁路模型请求（tools-more.md §4）：把正文与用户的 prompt 交给模型，取第一个文本块。
//! 请求来源 `QuerySource::Side("web_fetch_apply")`；不带 system 段与工具。

use futures_util::StreamExt;
use rust_cli_kernel_api::message::ContentBlock;
use rust_cli_kernel_api::model::{
    ApiContent, ApiMessage, ApiRole, BlockDelta, ModelEvent, ModelRequest, QuerySource,
    RequestOptions, small_fast_model,
};
use rust_cli_kernel_api::tool::{ToolError, ToolUseContext};

use crate::common::failed;
use crate::web_fetch::{APPLY_LIMIT, SideModel};
use crate::web_texts as texts;

/// 旁路请求的输出上限（取一个保守值，§7）。
const MAX_TOKENS: u32 = 8_192;

pub(crate) async fn apply_prompt(
    side: Option<&SideModel>,
    prompt: &str,
    content: &str,
    preapproved: bool,
    lead: &str,
    context: &ToolUseContext,
) -> Result<String, ToolError> {
    let Some(side) = side else {
        return Err(failed("no model is configured for WebFetch".to_owned()));
    };
    let body = if content.chars().count() > APPLY_LIMIT {
        let head: String = content.chars().take(APPLY_LIMIT).collect();
        format!("{head}{}", texts::CONTENT_TRUNCATED)
    } else {
        content.to_owned()
    };
    let user = format!(
        "{lead}{}",
        texts::secondary_prompt(&body, prompt, preapproved)
    );
    // 小模型槽位（model.md §6.1）：会话当前的主模型向会话 actor 取；没有会话 owner 时用组装时的回退值。
    let main = context
        .interactions
        .current_selection(&context.session)
        .await
        .unwrap_or_else(|| side.selection.clone());
    let request = ModelRequest {
        selection: small_fast_model(&main),
        system: Vec::new(),
        messages: vec![ApiMessage {
            role: ApiRole::User,
            content: vec![ApiContent {
                block: ContentBlock::Text { text: user },
                cache_control: None,
            }],
            output_config: None,
        }],
        tools: Vec::new(),
        tool_choice: None,
        max_tokens: MAX_TOKENS,
        thinking: None,
        temperature: None,
        betas: Vec::new(),
        // 名字只在 kernel-api 定义一处（用量表与请求头写成 Node 的 `web_fetch_processing`，工作线 F71）。
        source: QuerySource::Side(rust_cli_kernel_api::model::SIDE_WEB_FETCH.to_owned()),
        // 工作线 F52：`metadata.user_id` 的 `session_id`（model.md §3.3）。
        options: RequestOptions {
            session_id: Some(context.session.clone()),
            // 工作线 F64：每请求头的回合与子代理（model.md §2.4）。
            run: Some(context.run.clone()),
            agent: context.agent.clone(),
            ..RequestOptions::default()
        },
    };
    let mut stream = side.model.stream(request, context.cancel.clone());
    // 只取第一个块的文本（`content[0].text`）。
    let mut first: Option<usize> = None;
    let mut text = String::new();
    while let Some(event) = stream.next().await {
        match event.map_err(|error| failed(error.to_string()))? {
            ModelEvent::BlockStart { index, block } if first.is_none() => {
                first = Some(index);
                if let ContentBlock::Text { text: start } = block {
                    text.push_str(&start);
                }
            }
            ModelEvent::BlockDelta {
                index,
                delta: BlockDelta::Text(delta),
            } if first == Some(index) => text.push_str(&delta),
            _ => {}
        }
    }
    if context.cancel.is_cancelled() {
        return Err(ToolError::Cancelled);
    }
    if first.is_none() {
        return Ok(texts::NO_MODEL_RESPONSE.to_owned());
    }
    Ok(text)
}
