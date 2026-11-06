//! WebFetch 的旁路模型请求（tools-more.md §4）：把正文与用户的 prompt 交给模型，取第一个文本块。
//! 请求来源 `SideSource::WebFetch`；不带 system 段与工具。

use rust_cli_kernel_api::model::{ApiMessage, ModelRequest, small_fast_model};
use rust_cli_kernel_api::side_request::{Requester, SideReply, SideSource};
use rust_cli_kernel_api::tool::{ToolError, ToolUseContext};

use crate::web_fetch::{APPLY_LIMIT, SideModel};
use crate::web_texts as texts;
use rust_cli_kernel_api::tool::failed;

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
        .session_ops
        .view()
        .await
        .and_then(|view| view.selection)
        .unwrap_or_else(|| side.selection.current());
    let request = ModelRequest::side(
        small_fast_model(&main),
        SideSource::WebFetch,
        Vec::new(),
        vec![ApiMessage::user_text(user)],
        MAX_TOKENS,
        // 工作线 F52：`metadata.user_id` 的 `session_id`（model.md §3.3）；工作线 F64：每请求头的回合与子代理（model.md §2.4）。
        Requester {
            session: context.session.clone(),
            run: Some(context.run.clone()),
            agent: context.agent.clone(),
        },
    );
    let reply = SideReply::collect(side.model.stream(request, context.cancel.clone()))
        .await
        .map_err(|error| failed(error.to_string()))?;
    if context.cancel.is_cancelled() {
        return Err(ToolError::Cancelled);
    }
    // 只取第一个块的文本（`content[0].text`）。
    Ok(reply
        .first_text()
        .map_or_else(|| texts::NO_MODEL_RESPONSE.to_owned(), str::to_owned))
}
