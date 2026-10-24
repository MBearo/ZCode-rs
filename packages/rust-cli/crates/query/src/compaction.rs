//! 主循环里的压缩（docs/specs/rust-cli/compact.md §1、§4.1）：每一步发请求前的自动压缩，
//! 模型返回上下文超限后的反应式压缩。判定与压缩都在压缩器（[`Compactor`]）里，这里只负责调用、
//! 把边界、摘要与附件在一个事务里提交，并放进本次运行的历史。

use rust_cli_kernel_api::compaction::prompt_too_long_gap;
use rust_cli_kernel_api::message::{CompactTrigger, Message};
use rust_cli_kernel_api::model::{ModelSelection, QuerySource};
use rust_cli_kernel_api::orchestration::{
    AutoCompactOutcome, CompactRequest, CompactResult, CompactTracking, Compactor, QueryError,
    QueryStop,
};

use crate::RunScope;
use crate::recovery::QUIET_SIDE_SOURCES;

/// 一次运行内的压缩状态（自动压缩的跟踪与是否试过反应式压缩）。
#[derive(Debug, Default)]
pub(crate) struct Compaction {
    tracking: CompactTracking,
    reactive_tried: bool,
}

/// `compact` 来源与安静的旁路来源不做自动与反应式压缩。
fn eligible(source: &QuerySource) -> bool {
    match source {
        QuerySource::MainThread | QuerySource::Subagent(_) => true,
        QuerySource::Compact => false,
        QuerySource::Side(name) => !QUIET_SIDE_SOURCES.contains(&name.as_str()),
    }
}

fn request(
    scope: &RunScope,
    history: &[Message],
    selection: &ModelSelection,
    trigger: CompactTrigger,
) -> CompactRequest {
    CompactRequest {
        session: scope.session.clone(),
        workspace: scope.workspace.clone(),
        trigger,
        selection: selection.clone(),
        instructions: None,
        messages: history.to_vec(),
        mode: Some(scope.mode),
        agent: scope.agent.clone(),
        initial_token_gap: None,
    }
}

/// 边界、摘要与附件在一个事务里提交，再放进本次运行的历史。
async fn commit(
    scope: &RunScope,
    history: &mut Vec<Message>,
    result: CompactResult,
) -> Result<(), QueryError> {
    let CompactResult {
        boundary,
        summary,
        restored,
    } = result;
    let mut messages = vec![
        Message::System(rust_cli_kernel_api::message::SystemMessage::CompactBoundary(boundary)),
        Message::User(summary),
    ];
    messages.extend(restored);
    scope.sink.commit_messages(messages.clone()).await?;
    history.extend(messages);
    Ok(())
}

impl Compaction {
    /// 进入下一个工具轮。
    pub(crate) fn next_turn(&mut self) {
        self.tracking.next_turn();
        self.reactive_tried = false;
    }

    /// 发请求前的自动压缩与阻塞线；rapid refill 熔断、到了阻塞线时返回结束原因。
    pub(crate) async fn before_request(
        &mut self,
        scope: &RunScope,
        history: &mut Vec<Message>,
        selection: &ModelSelection,
        source: &QuerySource,
    ) -> Result<Option<QueryStop>, QueryError> {
        let compactor: &dyn Compactor = scope.parts.compactor.as_ref();
        let mut failure = None;
        // 先用便宜的同步判定把关：每一步都构造压缩请求要复制整个历史（README §8）。
        if eligible(source) && compactor.should_auto_compact(history, selection) {
            let outcome = compactor
                .auto_compact(
                    request(scope, history, selection, CompactTrigger::Auto),
                    &mut self.tracking,
                    scope.abort.token().clone(),
                )
                .await
                .map_err(|error| QueryError::Unsupported {
                    what: error.to_string(),
                })?;
            match outcome {
                AutoCompactOutcome::NotNeeded => {}
                // 失败已计入熔断次数，再看阻塞线（错误文字带上失败的原因）。
                AutoCompactOutcome::Failed { message } => failure = Some(message),
                AutoCompactOutcome::Thrashing { message } => {
                    return Ok(Some(QueryStop::ApiError { message }));
                }
                // 刚压缩过：这一步不看阻塞线。
                AutoCompactOutcome::Compacted { result } => {
                    // kernel-api 没有压缩进度回调：自动压缩的开始事件在摘要完成后才发（spec §8）。
                    scope.sink.compact_started(CompactTrigger::Auto).await;
                    commit(scope, history, *result).await?;
                    return Ok(None);
                }
            }
        }
        // 阻塞线（compact.md §2.2）：到了就不发请求，回合以错误结束；摘要请求不看。
        if *source != QuerySource::Compact
            && let Some(message) = compactor.blocking_limit(history, selection, failure.as_deref())
        {
            return Ok(Some(QueryStop::ApiError { message }));
        }
        Ok(None)
    }

    /// 上下文超限后的反应式压缩；压缩成功返回真，主循环重发请求。
    /// `error` 是 `ModelError::PromptTooLong` 的文本。
    pub(crate) async fn after_prompt_too_long(
        &mut self,
        scope: &RunScope,
        history: &mut Vec<Message>,
        selection: &ModelSelection,
        source: &QuerySource,
        error: &str,
    ) -> Result<bool, QueryError> {
        if self.reactive_tried || !eligible(source) || scope.abort.is_aborted() {
            return Ok(false);
        }
        self.reactive_tried = true;
        scope.sink.compact_started(CompactTrigger::Reactive).await;
        // 缺陷修复（L25）：原来不带触发它的错误给出的 token 差，压缩核心第一次就转全部摘要时
        // 把整段对话发出去；应从错误取差，作为压缩的初始差，据此裁掉开头。
        let mut compact = request(scope, history, selection, CompactTrigger::Reactive);
        compact.initial_token_gap = prompt_too_long_gap(error);
        let compacted = scope
            .parts
            .compactor
            .compact(compact, scope.abort.token().clone())
            .await;
        match compacted {
            Ok(result) => {
                commit(scope, history, result).await?;
                Ok(true)
            }
            // 压缩不了：主循环照旧以上下文超限的错误结束（放出被扣下的 413 错误）。
            Err(_) => Ok(false),
        }
    }
}

#[cfg(test)]
#[path = "compaction_tests.rs"]
mod tests;
