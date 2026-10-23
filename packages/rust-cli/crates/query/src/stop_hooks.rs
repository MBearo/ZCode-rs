//! Stop / SubagentStop hooks（docs/specs/rust-cli/hooks.md §7）：回合要正常结束时运行。hooks crate 已把要提交的文本渲染成消息，这里只提交并
//! 决定继续还是结束。

use rust_cli_kernel_api::hooks::{HookContext, HookEvent, HookEventName};
use rust_cli_kernel_api::message::{ContentBlock, Message};
use rust_cli_kernel_api::model::{ModelSelection, QuerySource};
use rust_cli_kernel_api::orchestration::QueryError;
use serde_json::{Map, Value, json};

use crate::RunScope;
use crate::respond::commit;

/// 连续阻塞的上限（缺省值）。
const BLOCK_CAP: u32 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StopHook {
    /// 照常结束（hook 阻止继续时，结束说明已提交）。
    End,
    /// hook 阻塞了结束：反馈已提交，再请求一次（`stop_hook_blocking`）。
    Continue,
}

/// 最后一条 assistant 的文本块用 `\n` 连接后去空白；没有文本时省略。
fn last_assistant_text(history: &[Message]) -> Option<String> {
    let assistant = history.iter().rev().find_map(|message| match message {
        Message::Assistant(assistant) => Some(assistant),
        _ => None,
    })?;
    let text = assistant
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    Some(text.trim().to_owned()).filter(|text| !text.is_empty())
}

/// 输入 JSON 的事件专属字段（键序固定）。
fn payload(scope: &RunScope, source: &QuerySource, active: bool, last: Option<String>) -> Value {
    let mut payload = Map::new();
    payload.insert("stop_hook_active".into(), json!(active));
    if let QuerySource::Subagent(agent_type) = source {
        let agent = scope.agent.as_ref().map(ToString::to_string);
        payload.insert("agent_id".into(), json!(agent.unwrap_or_default()));
        payload.insert("agent_transcript_path".into(), json!(""));
        payload.insert("agent_type".into(), json!(agent_type));
    }
    if let Some(last) = last {
        payload.insert("last_assistant_message".into(), json!(last));
    }
    payload.insert("background_tasks".into(), json!([]));
    payload.insert("session_crons".into(), json!([]));
    Value::Object(payload)
}

/// `active`：这一轮已经因 Stop hook 续过；`blocked`：此前连续阻塞的次数。`selection` 是这次运行的模型
/// （`/goal` 评估的对话记录按它 normalize）。
pub(crate) async fn stop_hooks(
    scope: &RunScope,
    history: &mut Vec<Message>,
    source: &QuerySource,
    selection: &ModelSelection,
    active: bool,
    blocked: u32,
) -> Result<StopHook, QueryError> {
    if scope.abort.is_aborted() {
        return Ok(StopHook::End);
    }
    let (name, agent_type) = match source {
        QuerySource::Subagent(agent_type) => {
            (HookEventName::SubagentStop, Some(agent_type.clone()))
        }
        _ => (HookEventName::Stop, None),
    };
    let event = HookEvent {
        name,
        session: scope.session.clone(),
        workspace: scope.workspace.clone(),
        payload: payload(scope, source, active, last_assistant_text(history)),
        context: HookContext {
            mode: Some(scope.mode),
            agent: scope.agent.clone(),
            agent_type,
            cwd: None,
            run: Some(scope.run.clone()),
            // 会话当前的主模型（owner 是会话 actor，model.md §6.1）。
            selection: scope.sink.current_selection(&scope.session).await,
        },
    };
    // 会话有目标时，目标的评估与配置的 Stop hooks 一起跑（会话级的 Stop hook，goal.md §4）。
    let goal = crate::goal::due(scope, source).await;
    let hooks = scope
        .parts
        .hooks
        .run(event.clone(), scope.abort.token().clone());
    let (outcome, checked) = match goal {
        Some(condition) => {
            let check = crate::goal::evaluate(scope, event, condition.clone(), history, selection);
            let (outcome, verdict) = futures_util::future::join(hooks, check).await;
            (outcome, Some((condition, verdict)))
        }
        None => (hooks.await, None),
    };
    let prevent = outcome.prevent_continuation;
    let mut block = !prevent && !outcome.messages.is_empty();
    for message in outcome.messages {
        commit(scope, history, message).await?;
    }
    // 中止时不报告判定（直接返回）。
    if let Some((condition, verdict)) = checked.filter(|_| !scope.abort.is_aborted()) {
        let not_met = crate::goal::report(scope, history, condition, verdict).await?;
        block |= not_met && !prevent;
    }
    if !block {
        return Ok(StopHook::End);
    }
    if blocked + 1 > BLOCK_CAP {
        // 应另外显示一条警告（`A hook blocked the turn from ending N consecutive times …`）；
        // 这里还没有只供显示的提示消息，先记日志（query.md §6）。
        tracing::warn!(
            blocked = blocked + 1,
            "stop hooks blocked the turn too many times"
        );
        return Ok(StopHook::End);
    }
    Ok(StopHook::Continue)
}

#[cfg(test)]
#[path = "stop_hooks_tests.rs"]
mod tests;
