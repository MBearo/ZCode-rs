//! 主循环本体。
//!
//! 运行开始：当步的 attachment → 输入的 UserPromptSubmit 结果。每一步：自动压缩 → 上下文宣布 → 规范化 →
//! 一次模型响应（见 [`crate::respond`]）→ 有工具结果就并入插话与通知、产生当步的 attachment、进入下一轮，
//! 否则按恢复分支（见 [`crate::recovery`]）重试或结束。transition、计数与 attachment 的两个时点见
//! docs/specs/rust-cli/query.md §1、§1.1。

use std::sync::Arc;

use rust_cli_kernel_api::abort::AbortReason;
use rust_cli_kernel_api::compaction::context_tokens;
use rust_cli_kernel_api::history;
use rust_cli_kernel_api::message::{
    AssistantMessage, AttachmentMessage, ContentBlock, Message, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::metering::{Metered, ModelCall};
use rust_cli_kernel_api::model::{
    ContextEdit, ModelError, ModelRequest, ModelSelection, QuerySource, RequestOptions,
    ThinkingConfig,
};
use rust_cli_kernel_api::orchestration::{
    Absorbed, ContextRequest, OrchestrationError, QueryError, QueryRequest, QueryStop,
};
use rust_cli_kernel_api::provider::ModelCapabilities;
use rust_cli_kernel_api::tool::PromptContext;
use rust_cli_prompts::models;
use serde_json::json;

use crate::RunScope;
use crate::compaction::Compaction;
use crate::deferred::{ToolOrder, step_tool_search, tool_references, wire_tools};
use crate::limits::RequestLimits;
use crate::normalize::{NormalizeOptions, normalize};
use crate::recovery::{Fate, Guards, Outcome};
use crate::respond::{Responded, commit, commit_metered, mark_interrupted, respond};
use crate::step::StepTools;
use crate::stop_hooks::{StopHook, stop_hooks};
use crate::texts;

/// 随模型变化的请求参数（该模型的默认值：
/// `max_tokens`、`thinking {adaptive, display: updates}`、`context_management` 的
/// `clear_thinking keep all`、`output_config.effort`）。
struct ModelShape {
    max_tokens: u32,
    thinking: Option<ThinkingConfig>,
    options: RequestOptions,
    normalize: NormalizeOptions,
}

/// `capabilities` 是这一步模型的生效配置（model.md §11）：对话中间的 system 只看它，答不出时按卡片回退。卡片字段
/// （thinking、effort、context_management、每轮 effort）只来自卡片；请求方案决定发不发（model.md §3.2）。
/// `limits` 是入口的覆盖（修复线 F42，query.md §1.5）：`max_tokens` 由值、缺省与上限算出；thinking 关时不发 thinking 与
/// clear_thinking。
fn model_shape(
    selection: &ModelSelection,
    capabilities: Option<&ModelCapabilities>,
    limits: &RequestLimits,
) -> ModelShape {
    // 行为形状：有卡片的模型用自己的卡片，GLM 借 opus 的（工作线 F121，model.md §11.1）；上限只按卡片或目录。
    let shape = models::shape(&selection.model);
    let adaptive = shape.is_some_and(|shape| shape.adaptive_thinking) && limits.thinking_allowed();
    // 修 bug（F23）：没有卡片的模型以前也写选择的档位（`models::card_effort` 的注释）。
    let effort = models::card_effort(&selection.model, selection.reasoning.as_deref());
    let output_limit = models::card_output_limit(
        &selection.model,
        capabilities.and_then(|found| found.max_output_tokens),
    );
    ModelShape {
        max_tokens: limits.max_tokens(output_limit),
        thinking: adaptive.then(|| ThinkingConfig::Adaptive {
            display: Some("updates".to_owned()),
        }),
        options: RequestOptions {
            effort: effort.clone(),
            context_edits: if adaptive {
                vec![ContextEdit::ClearThinking {
                    keep: "all".to_owned(),
                }]
            } else {
                Vec::new()
            },
            // device_id 与上一次响应的 message id 由工作线 L6 / L1 补上。
            user_id: None,
            previous_message_id: None,
            ..RequestOptions::default()
        },
        normalize: NormalizeOptions {
            system_turns: models::mid_conversation_system(
                &selection.model,
                capabilities.map(|found| found.supports_mid_conversation_system),
            ),
            turn_effort: effort.filter(|_| shape.is_some_and(|shape| shape.per_turn_effort)),
            skip_cache_write: false,
        },
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
        let (text, data, after) = match item {
            Absorbed::Prompt(steered) => {
                let text = texts::queued_prompt(&steered.text);
                let data = json!({ "prompt": steered.text, "commandMode": "prompt" });
                (text, data, steered.after)
            }
            Absorbed::Notice(notice) => {
                let data = json!({ "prompt": notice, "commandMode": "task-notification" });
                (notice, data, Vec::new())
            }
        };
        let attachment = Message::Attachment(AttachmentMessage {
            id: scope.parts.ids.message(),
            source: "queued_command".to_owned(),
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

fn orchestration(error: OrchestrationError) -> QueryError {
    QueryError::Unsupported {
        what: error.to_string(),
    }
}

/// 组装上下文的请求：会话当前的模式、计划模式标志与工作区（actor 的同一个快照）；工具名与 MCP 状态出自这一步的
/// 工具快照（query.md §1.3）。
async fn context_request(
    scope: &RunScope,
    step: &StepTools,
    (selection, capabilities): (&ModelSelection, Option<ModelCapabilities>),
    source: &QuerySource,
) -> ContextRequest {
    let (mode, plan_flags) = scope.current_mode_and_plan_flags().await;
    ContextRequest {
        capabilities,
        session: scope.session.clone(),
        workspace: scope.current_workspace().await,
        agent: scope.agent.clone(),
        source: source.clone(),
        mode,
        tool_names: step.names(),
        selection: selection.clone(),
        plan_flags,
        mcp: step.mcp.clone(),
        // 远端执行 F98（remote-exec.md §25.2.1）：机器清单提醒按这次运行的缺省机器写（子代理可能缺省在远端）。
        default_machine: scope.default_machine_name().await,
    }
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
async fn commit_turn_attachments(
    scope: &RunScope,
    history: &mut Vec<Message>,
    (selection, source): (&ModelSelection, &QuerySource),
    (point, step): (TurnPoint, &StepTools),
) -> Result<(), QueryError> {
    let capabilities = scope.parts.model.capabilities(selection).await;
    let request = context_request(scope, step, (selection, capabilities), source).await;
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
    // `responses` 给前端区分流式增量属于哪次响应；`turn_count` 是轮数。
    let mut responses: usize = 0;
    let mut turn_count: u32 = 1;
    let mut guards = Guards::default();
    // Stop hook 的状态（是否在 Stop hook 之后、连续阻塞的次数）。
    let mut stop_hook_active = false;
    let mut stop_blocks: u32 = 0;
    let mut compaction = Compaction::default();
    // 退出（`Shutdown`，修复线 F38，query.md §1.2）：之后这个回合什么都不写。
    let shutdown = |scope: &RunScope| scope.abort.reason() == Some(AbortReason::Shutdown);
    if shutdown(&scope) {
        return Ok(QueryStop::Aborted);
    }
    // 这一步的工具快照（query.md §1.3）：运行开始与每个工具批之后各取一次，附件与请求共用。
    let mut step = StepTools::refresh(&scope, &selection).await;
    // 运行开始：输入那一步的当步 attachment，再提交输入的 UserPromptSubmit 结果（先产生当步 attachment、
    // 再跑 UPS；query.md §1.1）。
    commit_turn_attachments(
        &scope,
        &mut history,
        (&selection, &source),
        (TurnPoint::Start, &step),
    )
    .await?;
    if !prompt_hook_messages.is_empty() {
        scope
            .sink
            .commit_messages(prompt_hook_messages.clone())
            .await?;
        history.extend(prompt_hook_messages);
    }
    loop {
        // 自动压缩排在上下文宣布之前：压缩后按“边界之后”重新宣布规则文件与日期，
        // 环境段与模型身份在压缩附件里。已中止时不压缩（不为要被取消的回合发摘要请求，query.md §1.2）。
        if !scope.abort.is_aborted()
            && let Some(stop) = compaction
                .before_request(&scope, &mut history, &selection, &source)
                .await?
        {
            return Ok(stop);
        }
        if shutdown(&scope) {
            return Ok(QueryStop::Aborted);
        }
        // 这一步模型的能力（生效配置，model.md §11）：工具清单、system 提醒的措辞与对话中间的 system 都看它。
        let capabilities = scope.parts.model.capabilities(&selection).await;
        let tools = &step.tools;
        let tool_names = step.names();
        let request = context_request(&scope, &step, (&selection, capabilities), &source).await;
        let mode = request.mode;
        let context = scope
            .parts
            .context
            .build(request, &history)
            .await
            .map_err(orchestration)?;
        for attachment in context.attachments {
            let message = Message::Attachment(attachment);
            scope.sink.commit_message(message.clone()).await?;
            history.push(message);
        }
        // 修 bug（F37，query.md §1.2）：以前在这一步开头看到中止就返回，什么都不写；会话在输入还没提交时被中断
        // （展开或 UserPromptSubmit 期间）时照常提交输入、带着已中止的信号开主循环，这一轮就没有中断标记。正确的时序是
        // 发请求之前不看中止，请求立即失败、写中断标记，上下文宣布已经在前面产生。这里不发请求，宣布之后写同样的标记。
        if scope.abort.is_aborted() {
            mark_interrupted(&scope, &mut history, false).await?;
            return Ok(QueryStop::Aborted);
        }
        let prompt = PromptContext { tool_names, mode };
        // 只发活动历史：最后一个压缩边界之后，保留段接回摘要之后（compact.md §5）。
        let active = history::active(&history);
        // 目录模型的输出预算（修复线 F108，query.md §1.11）：按这一步的活动历史估算输入（与压缩的判定同一个函数），
        // 交给补丁的上限按窗口余量压低。
        let estimate = context_tokens(&active.iter().map(AsRef::as_ref).collect::<Vec<_>>());
        let provider_max_output_tokens = crate::output_budget::provider_output_limit(
            capabilities.as_ref(),
            scope.parts.limits.max_output_tokens,
            &source,
            estimate,
        );
        // 延迟工具与 ToolSearch（tools-more.md §3.3）：排序、按需加载、占位工具与 `defer_loading`。这一步开不开看工具里有没有
        // ToolSearch 与 `ENABLE_TOOL_SEARCH`（自动时按门槛，工作线 F89，§3.2.1）；开着时的形式按端点的能力（§3.2.2：产品
        // 路径一律本地，F89；F84 时 z.ai / bigmodel 原生）。
        let tool_search = step_tool_search(
            tools,
            &prompt,
            (
                scope.parts.limits.tool_search,
                &selection.model,
                capabilities.as_ref(),
            ),
        );
        // 子代理点名的工具清单按清单的顺序发（agents.md §4.2，F16），其余按名字排序。
        let order = if scope.parts.tools.keeps_order() {
            ToolOrder::AsGiven
        } else {
            ToolOrder::ByName
        };
        let history_iter = active.iter().map(AsRef::as_ref);
        let mut wired = wire_tools(tools, history_iter, &prompt, (tool_search, order));
        // 有远端机器连上时带 `machine` 参数的工具多这一项（R3，remote-exec.md §20.5.1）；没有时不变。
        let default_machine = scope.default_machine_name().await;
        crate::machines::add_machine_param(
            &mut wired.definitions,
            &scope.parts.machines.machines(),
            default_machine.as_deref(),
        );
        step.declare(&scope, &source);
        let active: Vec<_> = active
            .iter()
            .map(|message| tool_references(message, wired.mode, &prompt.tool_names))
            .collect();
        let user_context: Vec<Message> = context
            .user_context
            .into_iter()
            .map(Message::User)
            .collect();
        let mut shape = model_shape(&selection, capabilities.as_ref(), &scope.parts.limits);
        // 摘要请求不写缓存（context.md §3 第 7 条）。
        shape.normalize.skip_cache_write = source == QuerySource::Compact;
        let max_tokens = shape.max_tokens;
        let model_request = ModelRequest {
            selection: selection.clone(),
            system: context.system,
            messages: normalize(
                user_context.iter().chain(active.iter().map(AsRef::as_ref)),
                &shape.normalize,
            ),
            tools: wired.definitions,
            tool_choice: None,
            max_tokens: shape.max_tokens,
            thinking: shape.thinking,
            temperature: None,
            betas: Vec::new(),
            source: source.clone(),
            options: RequestOptions {
                tool_flags: wired.flags,
                // 工作线 F52（model.md §3.3）：发请求的会话（子代理是子会话），模型适配器据此写 `metadata.user_id`。
                session_id: Some(scope.session.clone()),
                // 工作线 F64（model.md §2.4）：这次运行（回合）与子代理，模型适配器据此写每请求头。
                run: Some(scope.run.clone()),
                agent: scope.agent.clone(),
                provider_max_output_tokens,
                ..shape.options
            },
        };
        let responded = match respond(&scope, &mut history, model_request, responses).await {
            // 上下文超限的反应式压缩排在其他恢复分支之前（query.md §1）；压缩成功就重发，不计轮数。
            Err(QueryError::Model(ModelError::PromptTooLong { message })) => {
                if compaction
                    .after_prompt_too_long(&scope, &mut history, &selection, &source, &message)
                    .await?
                {
                    continue;
                }
                return Err(QueryError::Model(ModelError::PromptTooLong { message }));
            }
            other => other?,
        };
        responses += 1;
        match responded {
            Responded::Aborted => return Ok(QueryStop::Aborted),
            Responded::ToolsFinished => {
                turn_count += 1;
                guards.next_turn();
                // 进入下一轮：连续阻塞计数清零，Stop hook 的状态保留。
                stop_blocks = 0;
                compaction.next_turn();
                // 工具批之后先刷新工具与 MCP 状态，再算附件（query.md §1.3）。
                step = StepTools::refresh(&scope, &selection).await;
                // 工具轮收尾：到了轮数上限时不并入排队的输入（`deliverable` 为空），
                // 当步的 attachment 照常产生，再提交 `max_turns_reached`。
                if over_max_turns(max_turns, turn_count).is_none() {
                    absorb_steering(&scope, &mut history).await?;
                }
                let turn = (&selection, &source);
                let point = (TurnPoint::ToolRound, &step);
                commit_turn_attachments(&scope, &mut history, turn, point).await?;
                if reached_max_turns(&scope, &mut history, max_turns, turn_count).await? {
                    return Ok(QueryStop::MaxTurns);
                }
            }
            Responded::Done(assistant, call) => {
                let outcome = guards.decide(&assistant, &source, max_tokens);
                match conclude(&scope, &mut history, outcome, assistant, call).await? {
                    // 要正常结束：先跑 Stop hooks（hooks.md §7），它们可以让回合继续。
                    Some(QueryStop::Completed) => {
                        let hook = stop_hooks(
                            &scope,
                            &mut history,
                            &source,
                            &selection,
                            stop_hook_active,
                            stop_blocks,
                        )
                        .await?;
                        if hook == StopHook::End {
                            return Ok(QueryStop::Completed);
                        }
                        stop_hook_active = true;
                        stop_blocks += 1;
                        turn_count += 1;
                        guards.after_stop_block();
                        if reached_max_turns(&scope, &mut history, max_turns, turn_count).await? {
                            return Ok(QueryStop::MaxTurns);
                        }
                    }
                    Some(stop) => return Ok(stop),
                    None => {}
                }
            }
            Responded::Truncated {
                assistant,
                error,
                call,
            } => {
                let non_interactive = scope.parts.non_interactive;
                let outcome = guards.truncated(&assistant, &source, non_interactive, &error);
                if let Some(stop) = conclude(&scope, &mut history, outcome, assistant, call).await?
                {
                    return Ok(stop);
                }
            }
            // 只交出了 thinking 之后的流内失败（provider-errors.md §3.3，D11 暂定，修复线 F113）：次数没满时不提交这次
            // 回复（计量单独提交）、回复序号已加 1、重发同一个请求；满了照部分收尾。
            Responded::ThinkingOnly {
                assistant,
                error,
                call,
            } => {
                if guards.resend_after_thinking(&error) {
                    scope
                        .sink
                        .commit_metered(None, Metered::Model(call))
                        .await?;
                    continue;
                }
                if assistant.content.is_empty() {
                    scope
                        .sink
                        .commit_metered(None, Metered::Model(call))
                        .await?;
                    return Err(error.into());
                }
                let non_interactive = scope.parts.non_interactive;
                let outcome = guards.truncated(&assistant, &source, non_interactive, &error);
                if let Some(stop) = conclude(&scope, &mut history, outcome, assistant, call).await?
                {
                    return Ok(stop);
                }
            }
        }
    }
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
