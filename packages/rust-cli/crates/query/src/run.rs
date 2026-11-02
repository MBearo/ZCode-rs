//! 主循环本体。
//!
//! 运行开始：当步的 attachment → 输入的 UserPromptSubmit 结果。每一步：自动压缩 → 准备请求（上下文宣布与组装，见
//! [`crate::request`]）→ 一次模型响应（见 [`crate::respond`]）→ 有工具结果就进入工具轮的收尾（并入插话与通知、产生当步的
//! attachment），否则按恢复分支（见 [`crate::recovery`]）与 Stop hooks 重试或结束。transition、计数与 attachment 的两个
//! 时点见 docs/specs/rust-cli/query.md §1、§1.1、§2。
//!
//! 工作线 F131：以前 `run` 一个函数 266 行，计数是 6 个一起改的局部变量；现在计数收进 [`LoopState`]，各阶段各一个函数，
//! 请求的字节与提交的顺序不变。

use std::sync::Arc;

use rust_cli_kernel_api::abort::AbortReason;
use rust_cli_kernel_api::hooks::queued_command;
use rust_cli_kernel_api::message::{
    AssistantMessage, AttachmentMessage, ContentBlock, Message, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::metering::{Metered, ModelCall};
use rust_cli_kernel_api::model::{ModelError, ModelSelection, QuerySource};
use rust_cli_kernel_api::orchestration::{Absorbed, QueryError, QueryRequest, QueryStop};
use serde_json::json;

use crate::RunScope;
use crate::compaction::Compaction;
use crate::recovery::{Fate, Guards, Outcome};
use crate::request::{Prepared, orchestration, prepare};
use crate::respond::{Responded, commit, commit_metered, respond};
use crate::step::StepTools;
use crate::stop_hooks::{StopHook, stop_hooks};
use crate::texts;

/// 一次运行内不变的请求参数。
struct Inputs {
    selection: ModelSelection,
    source: QuerySource,
    /// 每次提问最多几轮（`--max-turns`）；None 或 0 不限制。
    max_turns: Option<u32>,
}

impl Inputs {
    fn turn(&self) -> (&ModelSelection, &QuerySource) {
        (&self.selection, &self.source)
    }
}

/// 一次运行里跨迭代的状态（query.md §2 的计数）：工具轮与 Stop hook 续写时一起变。
struct LoopState {
    /// 前端区分流式增量属于哪次响应。
    responses: usize,
    /// 轮数（`turnCount`）。
    turn_count: u32,
    /// 恢复分支的计数。
    guards: Guards,
    /// 这一轮已经因 Stop hook 续过（`stop_hook_active`）。
    stop_hook_active: bool,
    /// 连续被 Stop hook 阻塞的次数。
    stop_blocks: u32,
    /// 自动压缩的跟踪与是否试过反应式压缩。
    compaction: Compaction,
}

impl LoopState {
    fn new() -> Self {
        Self {
            responses: 0,
            turn_count: 1,
            guards: Guards::default(),
            stop_hook_active: false,
            stop_blocks: 0,
            compaction: Compaction::default(),
        }
    }

    /// 工具结果之后进入下一轮：轮数加 1，恢复计数、连续阻塞次数与压缩的单轮状态清零，Stop hook 的状态保留。
    fn next_turn(&mut self) {
        self.turn_count += 1;
        self.guards.next_turn();
        self.stop_blocks = 0;
        self.compaction.next_turn();
    }

    /// Stop hook 阻塞了结束、再请求一次（`stop_hook_blocking`）：轮数与连续阻塞次数加 1，输出上限续写次数与 malformed
    /// 重试清零。
    fn after_stop_block(&mut self) {
        self.stop_hook_active = true;
        self.stop_blocks += 1;
        self.turn_count += 1;
        self.guards.after_stop_block();
    }
}

/// 没有 tool_use 的响应：按恢复分支的判定提交并决定下一步；`None` 表示重试。这次请求的计量与响应同批
/// 提交；响应不提交（丢弃、没有内容）时单独提交（usage.md §2.2）。
async fn conclude(
    scope: &RunScope,
    history: &mut Vec<Message>,
    outcome: Outcome,
    assistant: AssistantMessage,
    call: ModelCall,
) -> Result<Option<QueryStop>, QueryError> {
    let fate = match &outcome {
        Outcome::Retry { fate, .. } => *fate,
        Outcome::Completed | Outcome::Failed { .. } => Fate::Keep,
    };
    let metered = Metered::Model(call);
    if assistant.content.is_empty() {
        scope.sink.commit_metered(None, metered).await?;
    } else {
        let message = Message::Assistant(assistant);
        match fate {
            Fate::Keep => commit_metered(scope, history, message, metered).await?,
            Fate::Hide => scope.sink.commit_metered(Some(message), metered).await?,
            Fate::Discard => scope.sink.commit_metered(None, metered).await?,
        }
    }
    match outcome {
        Outcome::Completed => Ok(Some(QueryStop::Completed)),
        Outcome::Failed { message } => Ok(Some(QueryStop::ApiError { message })),
        Outcome::Retry { nudge, source, .. } => {
            let message = Message::User(UserMessage {
                id: scope.parts.ids.message(),
                content: vec![ContentBlock::Text {
                    text: nudge.to_owned(),
                }],
                origin: UserOrigin::Meta {
                    source: source.to_owned(),
                },
            });
            commit(scope, history, message).await?;
            Ok(None)
        }
    }
}

/// 轮数是否用尽（轮数超过上限；0 等于不限制），用尽时给出上限。
fn over_max_turns(max_turns: Option<u32>, turn_count: u32) -> Option<u32> {
    max_turns.filter(|max| *max > 0 && turn_count > *max)
}

/// 工具结果之后、进入下一轮之前的轮数检查（`max_turns_reached`）。
async fn reached_max_turns(
    scope: &RunScope,
    history: &mut Vec<Message>,
    max_turns: Option<u32>,
    turn_count: u32,
) -> Result<bool, QueryError> {
    let Some(max) = over_max_turns(max_turns, turn_count) else {
        return Ok(false);
    };
    let attachment = Message::Attachment(AttachmentMessage {
        id: scope.parts.ids.message(),
        source: "max_turns_reached".to_owned(),
        text: String::new(),
        data: json!({ "maxTurns": max, "turnCount": turn_count }),
    });
    commit(scope, history, attachment).await?;
    Ok(true)
}

/// 工具边界并入插话与后台通知（在工具结果之后按队列顺序吸收优先级 `next` 的排队命令，
/// `absorbed_mid_turn`；query.md §4、§5）。每条插话的 UserPromptSubmit 消息紧跟在它的 `queued_command`
/// 之后（query.md §4.1）；通知是 `commandMode: task-notification`（§5.1）。
async fn absorb_steering(scope: &RunScope, history: &mut Vec<Message>) -> Result<(), QueryError> {
    for item in scope.sink.take_absorbed().await {
        let (id, text, data, after) = match item {
            Absorbed::Prompt(steered) => {
                let text = texts::queued_prompt(&steered.text);
                let data = json!({
                    queued_command::PROMPT_KEY: steered.text,
                    queued_command::MODE_KEY: queued_command::PROMPT_MODE,
                });
                (None, text, data, steered.after)
            }
            // 输出端给了消息 ID 时用它：提交之后它按这个 ID 认出是哪一条通知（会话 actor 的通道，工作线 F130）。
            Absorbed::Notice(notice) => {
                let data = json!({
                    queued_command::PROMPT_KEY: notice.text,
                    queued_command::MODE_KEY: queued_command::TASK_NOTIFICATION,
                });
                (notice.id, notice.text, data, Vec::new())
            }
        };
        let attachment = Message::Attachment(AttachmentMessage {
            id: id.unwrap_or_else(|| scope.parts.ids.message()),
            source: queued_command::SOURCE.to_owned(),
            text,
            data,
        });
        commit(scope, history, attachment).await?;
        if !after.is_empty() {
            scope.sink.commit_messages(after.clone()).await?;
            history.extend(after);
        }
    }
    Ok(())
}

/// 当步的 attachment 在哪个时点产生（docs/specs/rust-cli/query.md §1.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnPoint {
    /// 运行开始：主线程是人类输入之后，子代理是进入子循环之前。
    Start,
    /// 工具轮结束。
    ToolRound,
}

/// 当步的 attachment：运行开始（人类输入之后）与每个工具轮结束时产生，
/// 落库并接进历史，排在下一次自动压缩之前（docs/specs/rust-cli/query.md §1.1）。
///
/// 修 bug（F10）：以前每一步先自动压缩、再一次组装全部 attachment，阈值压缩的摘要请求
/// 少了上一个工具轮的 `<total_tokens>`；人类输入的 UserPromptSubmit 上下文由会话先提交，排在环境段之前。
/// 应在工具轮结束时与处理输入时（UPS 之前）就产生这一组，这里照做。
///
/// 修 bug（F16）：子代理以前在每一步的 `build` 里给出全部 attachment，工具轮之后少 agent
/// 清单与 `<total_tokens>`。现在运行开始调 `start_attachments`（子代理给出进入子循环前的那几项），
/// 工具轮结束调 `turn_attachments`（与主线程同一个时点）。
/// 当步的 attachment：运行开始（人类输入之后）与每个工具轮结束时产生，
/// 落库并接进历史，排在下一次自动压缩之前（docs/specs/rust-cli/query.md §1.1）。
///
/// 修 bug（F10）：以前每一步先自动压缩、再一次组装全部 attachment，阈值压缩的摘要请求
/// 少了上一个工具轮的 `<total_tokens>`；人类输入的 UserPromptSubmit 上下文由会话先提交，排在环境段之前。
/// 应在工具轮结束时与处理输入时（UPS 之前）就产生这一组，这里照做。
///
/// 修 bug（F16）：子代理以前在每一步的 `build` 里给出全部 attachment，工具轮之后少 agent
/// 清单与 `<total_tokens>`。现在运行开始调 `start_attachments`（子代理给出进入子循环前的那几项），
/// 工具轮结束调 `turn_attachments`（与主线程同一个时点）。
async fn commit_turn_attachments(
    scope: &RunScope,
    history: &mut Vec<Message>,
    (selection, source): (&ModelSelection, &QuerySource),
    (point, step): (TurnPoint, &StepTools),
) -> Result<(), QueryError> {
    let request = step.context_request(scope, selection, source).await;
    let context = &scope.parts.context;
    let attachments = match point {
        TurnPoint::Start => context.start_attachments(request, history).await,
        TurnPoint::ToolRound => context.turn_attachments(request, history).await,
    }
    .map_err(orchestration)?;
    for attachment in attachments {
        let message = Message::Attachment(attachment);
        scope.sink.commit_message(message.clone()).await?;
        history.push(message);
    }
    Ok(())
}

/// 退出（`Shutdown`，修复线 F38，query.md §1.2）：之后这个回合什么都不写。
fn shutdown(scope: &RunScope) -> bool {
    scope.abort.reason() == Some(AbortReason::Shutdown)
}

pub(crate) async fn run(
    scope: Arc<RunScope>,
    request: QueryRequest,
) -> Result<QueryStop, QueryError> {
    let QueryRequest {
        messages: mut history,
        selection,
        source,
        max_turns,
        prompt_hook_messages,
        ..
    } = request;
    let inputs = Inputs {
        selection,
        source,
        max_turns,
    };
    if shutdown(&scope) {
        return Ok(QueryStop::Aborted);
    }
    // 这一步的工具快照（query.md §1.3）：运行开始与每个工具批之后各取一次，附件与请求共用。
    let mut step = StepTools::refresh(&scope, &inputs.selection).await;
    start(&scope, &mut history, &inputs, &step, prompt_hook_messages).await?;
    let mut state = LoopState::new();
    loop {
        // 自动压缩排在上下文宣布之前：压缩后按“边界之后”重新宣布规则文件与日期，
        // 环境段与模型身份在压缩附件里。已中止时不压缩（不为要被取消的回合发摘要请求，query.md §1.2）。
        if !scope.abort.is_aborted()
            && let Some(stop) = state
                .compaction
                .before_request(&scope, &mut history, &inputs.selection, &inputs.source)
                .await?
        {
            return Ok(stop);
        }
        if shutdown(&scope) {
            return Ok(QueryStop::Aborted);
        }
        let Some(Prepared {
            request,
            max_tokens,
        }) = prepare(&scope, &mut history, &step, inputs.turn()).await?
        else {
            return Ok(QueryStop::Aborted);
        };
        let responded = match respond(&scope, &mut history, request, state.responses).await {
            // 上下文超限的反应式压缩排在其他恢复分支之前（query.md §1）；压缩成功就重发，不计轮数。
            Err(QueryError::Model(ModelError::PromptTooLong { message })) => {
                if state
                    .compaction
                    .after_prompt_too_long(
                        &scope,
                        &mut history,
                        &inputs.selection,
                        &inputs.source,
                        &message,
                    )
                    .await?
                {
                    continue;
                }
                return Err(QueryError::Model(ModelError::PromptTooLong { message }));
            }
            other => other?,
        };
        state.responses += 1;
        let stop = match responded {
            Responded::Aborted => Some(QueryStop::Aborted),
            // 工具的 hook 要求停止：不并入插话、不产生当步的 attachment、不做轮数检查（query.md §3，缺陷 D3）。
            Responded::HookStopped => Some(QueryStop::HookStopped),
            Responded::ToolsFinished => {
                tool_round(&scope, &mut history, &inputs, &mut step, &mut state).await?
            }
            Responded::Done(assistant, call) => {
                let outcome = state.guards.decide(&assistant, &inputs.source, max_tokens);
                let ended = (outcome, assistant, call);
                reply_ended(&scope, &mut history, &inputs, &mut state, ended).await?
            }
            Responded::Truncated {
                assistant,
                error,
                call,
            } => {
                let non_interactive = scope.parts.non_interactive;
                let outcome =
                    (state.guards).truncated(&assistant, &inputs.source, non_interactive, &error);
                conclude(&scope, &mut history, outcome, assistant, call).await?
            }
            Responded::ThinkingOnly {
                assistant,
                error,
                call,
            } => {
                let partial = (assistant, error, call);
                thinking_only(&scope, &mut history, &inputs, &mut state, partial).await?
            }
        };
        if let Some(stop) = stop {
            return Ok(stop);
        }
    }
}

/// 运行开始：输入那一步的当步 attachment，再提交输入的 UserPromptSubmit 结果（先产生当步 attachment、再跑 UPS；
/// query.md §1.1）。
async fn start(
    scope: &RunScope,
    history: &mut Vec<Message>,
    inputs: &Inputs,
    step: &StepTools,
    prompt_hook_messages: Vec<Message>,
) -> Result<(), QueryError> {
    commit_turn_attachments(scope, history, inputs.turn(), (TurnPoint::Start, step)).await?;
    if !prompt_hook_messages.is_empty() {
        scope
            .sink
            .commit_messages(prompt_hook_messages.clone())
            .await?;
        history.extend(prompt_hook_messages);
    }
    Ok(())
}

/// 工具轮的收尾（工具结果都已提交）：进入下一轮，刷新工具与 MCP 状态，再算附件（query.md §1.3）。到了轮数上限时
/// 不并入排队的输入（`deliverable` 为空），当步的 attachment 照常产生，再提交 `max_turns_reached`。
async fn tool_round(
    scope: &RunScope,
    history: &mut Vec<Message>,
    inputs: &Inputs,
    step: &mut StepTools,
    state: &mut LoopState,
) -> Result<Option<QueryStop>, QueryError> {
    state.next_turn();
    *step = StepTools::refresh(scope, &inputs.selection).await;
    if over_max_turns(inputs.max_turns, state.turn_count).is_none() {
        absorb_steering(scope, history).await?;
    }
    commit_turn_attachments(scope, history, inputs.turn(), (TurnPoint::ToolRound, step)).await?;
    if reached_max_turns(scope, history, inputs.max_turns, state.turn_count).await? {
        return Ok(Some(QueryStop::MaxTurns));
    }
    Ok(None)
}

/// 没有 tool_use 的回复：按恢复分支提交并决定下一步；要正常结束时先跑 Stop hooks（hooks.md §7），它们可以让回合
/// 继续。None 表示再请求一次。
async fn reply_ended(
    scope: &RunScope,
    history: &mut Vec<Message>,
    inputs: &Inputs,
    state: &mut LoopState,
    (outcome, assistant, call): (Outcome, AssistantMessage, ModelCall),
) -> Result<Option<QueryStop>, QueryError> {
    match conclude(scope, history, outcome, assistant, call).await? {
        Some(QueryStop::Completed) => {
            let hook = stop_hooks(
                scope,
                history,
                &inputs.source,
                &inputs.selection,
                state.stop_hook_active,
                state.stop_blocks,
            )
            .await?;
            match hook {
                StopHook::End => return Ok(Some(QueryStop::Completed)),
                StopHook::Prevented => return Ok(Some(QueryStop::StopHookPrevented)),
                StopHook::Continue => {}
            }
            state.after_stop_block();
            if reached_max_turns(scope, history, inputs.max_turns, state.turn_count).await? {
                return Ok(Some(QueryStop::MaxTurns));
            }
            Ok(None)
        }
        other => Ok(other),
    }
}

/// 只交出了 thinking 之后的流内失败（provider-errors.md §3.3，D11 暂定，修复线 F113）：次数没满时不提交这次回复（计量
/// 单独提交）、回复序号已加 1、重发同一个请求；满了照部分收尾。None 表示再请求一次。
async fn thinking_only(
    scope: &RunScope,
    history: &mut Vec<Message>,
    inputs: &Inputs,
    state: &mut LoopState,
    (assistant, error, call): (AssistantMessage, ModelError, ModelCall),
) -> Result<Option<QueryStop>, QueryError> {
    if state.guards.resend_after_thinking(&error) {
        scope
            .sink
            .commit_metered(None, Metered::Model(call))
            .await?;
        return Ok(None);
    }
    if assistant.content.is_empty() {
        scope
            .sink
            .commit_metered(None, Metered::Model(call))
            .await?;
        return Err(error.into());
    }
    let non_interactive = scope.parts.non_interactive;
    let outcome = (state.guards).truncated(&assistant, &inputs.source, non_interactive, &error);
    conclude(scope, history, outcome, assistant, call).await
}

#[cfg(test)]
#[path = "run_tests.rs"]
pub(crate) mod tests;

#[cfg(test)]
#[path = "run_recovery_tests.rs"]
mod recovery_tests;

#[cfg(test)]
#[path = "run_notice_tests.rs"]
mod notice_tests;

#[cfg(test)]
#[path = "run_turn_tests.rs"]
mod turn_tests;

#[cfg(test)]
#[path = "run_capability_tests.rs"]
mod capability_tests;

#[cfg(test)]
#[path = "run_order_tests.rs"]
mod order_tests;

#[cfg(test)]
#[path = "run_limits_tests.rs"]
mod limits_tests;

#[cfg(test)]
#[path = "run_budget_tests.rs"]
mod budget_tests;
