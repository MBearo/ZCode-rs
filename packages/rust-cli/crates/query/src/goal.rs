//! `/goal` 的停止前检查（docs/specs/rust-cli/goal.md §4）：主线程要正常
//! 结束、会话有目标时，与配置的 Stop hooks 一起评估目标条件。判定交给会话 actor（目标的唯一 owner）：
//! actor 提交反馈与 `goal_status`、改目标，返回已提交的消息，这里并入主循环的历史。

use rust_cli_kernel_api::history;
use rust_cli_kernel_api::hooks::{HookEvent, StopCondition, StopVerdict};
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::model::{ApiMessage, ModelSelection, QuerySource};
use rust_cli_kernel_api::orchestration::QueryError;
use rust_cli_prompts::models;

use crate::RunScope;
use crate::normalize::{NormalizeOptions, normalize};

/// 这一步要检查的目标条件：只在主线程，没有目标时为 None。
pub(crate) async fn due(scope: &RunScope, source: &QuerySource) -> Option<String> {
    if scope.agent.is_some() || *source != QuerySource::MainThread {
        return None;
    }
    scope.sink.current_goal().await
}

/// 评估的对话记录：活动历史按发给模型的规则 normalize（对话中间的 system 消息照模型能力），不带每轮
/// effort，不含 user context。
fn transcript(
    history: &[Message],
    selection: &ModelSelection,
    configured: Option<bool>,
) -> Vec<ApiMessage> {
    let options = NormalizeOptions {
        // 生效配置优先，答不出时按卡片回退（model.md §11，修复线 F23）。
        system_turns: models::mid_conversation_system(&selection.model, configured),
        ..NormalizeOptions::default()
    };
    normalize(history::active(history).iter().map(AsRef::as_ref), &options)
}

/// 评估目标条件（hooks 的会话级 Stop prompt hook）。
pub(crate) async fn evaluate(
    scope: &RunScope,
    event: HookEvent,
    condition: String,
    history: &[Message],
    selection: &ModelSelection,
) -> StopVerdict {
    let configured = scope
        .parts
        .model
        .capabilities(selection)
        .await
        .map(|found| found.supports_mid_conversation_system);
    let request = StopCondition {
        event,
        condition,
        transcript: transcript(history, selection, configured),
    };
    scope
        .parts
        .hooks
        .evaluate_stop_condition(request, scope.abort.token().clone())
        .await
}

/// 报告判定，把 actor 提交的消息并入历史；返回目标是否阻塞了结束（未达成，且 actor 确实应用了它）。
pub(crate) async fn report(
    scope: &RunScope,
    history: &mut Vec<Message>,
    condition: String,
    verdict: StopVerdict,
) -> Result<bool, QueryError> {
    let not_met = matches!(verdict, StopVerdict::NotMet { .. });
    let committed = scope.sink.goal_checked(condition, verdict).await?;
    let blocked = not_met && !committed.is_empty();
    history.extend(committed);
    Ok(blocked)
}

#[cfg(test)]
#[path = "goal_tests.rs"]
mod tests;
