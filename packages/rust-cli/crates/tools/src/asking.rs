//! 工具里的用户询问（权限询问、AskUserQuestion、ExitPlanMode 共用，docs/specs/rust-cli/session-lifecycle.md §2）。

use rust_cli_kernel_api::orchestration::InteractionRequest;
use rust_cli_kernel_api::tool::ToolUseContext;
use serde::de::DeserializeOwned;

/// 工具在等回答时被取消（询问已撤回）。
pub(crate) struct Cancelled;

/// 发出询问，等回答或工具被取消。回答读成 `T`；交互失败或回答读不出时为 `None`（调用方按拒绝处理）。
///
/// 修复（R1-5）：以前三处各自 `select!` 等回答，取消时只丢掉 future——交互的 owner 不知道，待回答的交互要到回合
/// 结束才清：对话框一直显示，迟到的回答还会提交“总是允许”。现在取消时先撤回询问（`Interactions::cancel_ask`，
/// 取消是一条消息），再返回。
pub(crate) async fn ask<T: DeserializeOwned>(
    context: &ToolUseContext,
    request: InteractionRequest,
) -> Result<Option<T>, Cancelled> {
    let tool_use = request.tool_use().clone();
    let answer = tokio::select! {
        answer = context.interactions.ask(&context.session, request) => answer,
        () = context.cancel.cancelled() => {
            context.interactions.cancel_ask(&context.session, &tool_use).await;
            return Err(Cancelled);
        }
    };
    Ok(answer
        .ok()
        .and_then(|answer| serde_json::from_value(answer.answer).ok()))
}

#[cfg(test)]
#[path = "asking_tests.rs"]
mod tests;
