//! 一步的“准备请求”（docs/specs/rust-cli/query.md §1）：上下文宣布落库，按这一步的工具快照与模型组装请求。
//! 请求的字节由这里决定（逐请求的轨迹比对看它）；工作线 F131 从 `run.rs` 拆出，计算与顺序不变。

use rust_cli_kernel_api::compaction::context_tokens;
use rust_cli_kernel_api::history;
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::model::{
    ContextEdit, ModelRequest, ModelSelection, QuerySource, RequestOptions, SystemBlock,
    ThinkingConfig,
};
use rust_cli_kernel_api::orchestration::{OrchestrationError, QueryError};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::provider::ModelCapabilities;
use rust_cli_kernel_api::tool::PromptContext;
use rust_cli_prompts::models;

use crate::RunScope;
use crate::deferred::{ToolOrder, step_tool_search, tool_references, wire_tools};
use crate::limits::RequestLimits;
use crate::normalize::{NormalizeOptions, normalize};
use crate::respond::mark_interrupted;
use crate::step::StepTools;

pub(crate) fn orchestration(error: OrchestrationError) -> QueryError {
    QueryError::Unsupported {
        what: error.to_string(),
    }
}

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

/// 这一步的请求与它的输出上限（没有 tool_use 时按它判定输出上限的续写）。
pub(crate) struct Prepared {
    pub(crate) request: ModelRequest,
    pub(crate) max_tokens: u32,
}

/// 上下文宣布（规则文件、会话上下文、日期、署名）落库，再组装请求。已中止时写中断标记、返回 None。
pub(crate) async fn prepare(
    scope: &RunScope,
    history: &mut Vec<Message>,
    step: &StepTools,
    (selection, source): (&ModelSelection, &QuerySource),
) -> Result<Option<Prepared>, QueryError> {
    let request = step.context_request(scope, selection, source).await;
    let mode = request.mode;
    let context = scope
        .parts
        .context
        .build(request, history)
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
        mark_interrupted(scope, history, false).await?;
        return Ok(None);
    }
    let context = (context.system, mode);
    Ok(Some(
        assemble(scope, history, step, (selection, source), context).await,
    ))
}

/// 按这一步的工具快照、模型与活动历史组装请求：工具清单与 ToolSearch 的形式、`machine` 参数、规范化的消息、随模型
/// 变化的参数与输出预算。
async fn assemble(
    scope: &RunScope,
    history: &[Message],
    step: &StepTools,
    (selection, source): (&ModelSelection, &QuerySource),
    (system, mode): (Vec<SystemBlock>, PermissionMode),
) -> Prepared {
    let prompt = PromptContext {
        tool_names: step.names(),
        mode,
    };
    let capabilities = step.capabilities.as_ref();
    // 只发活动历史：最后一个压缩边界之后，保留段接回摘要之后（compact.md §5）。
    let active = history::active(history);
    // 目录模型的输出预算（修复线 F108，query.md §1.11）：按这一步的活动历史估算输入（与压缩的判定同一个函数），
    // 交给补丁的上限按窗口余量压低。
    let estimate = context_tokens(&active.iter().map(AsRef::as_ref).collect::<Vec<_>>());
    let provider_max_output_tokens = crate::output_budget::provider_output_limit(
        capabilities,
        scope.parts.limits.max_output_tokens,
        source,
        estimate,
    );
    // 延迟工具与 ToolSearch（tools-more.md §3.3）：排序、按需加载、占位工具与 `defer_loading`。这一步开不开看工具里有没有
    // ToolSearch 与 `ENABLE_TOOL_SEARCH`（自动时按门槛，工作线 F89，§3.2.1）；开着时的形式按端点的能力（§3.2.2：产品
    // 路径一律本地，F89；F84 时 z.ai / bigmodel 原生）。
    let tool_search = step_tool_search(
        &step.tools,
        &prompt,
        (
            scope.parts.limits.tool_search,
            &selection.model,
            capabilities,
        ),
    );
    // 子代理点名的工具清单按清单的顺序发（agents.md §4.2，F16），其余按名字排序。
    let order = if scope.parts.tools.keeps_order() {
        ToolOrder::AsGiven
    } else {
        ToolOrder::ByName
    };
    let history_iter = active.iter().map(AsRef::as_ref);
    let mut wired = wire_tools(&step.tools, history_iter, &prompt, (tool_search, order));
    // 有远端机器连上时带 `machine` 参数的工具多这一项（R3，remote-exec.md §20.5.1）；没有时不变。
    crate::machines::add_machine_param(
        &mut wired.definitions,
        &step.tools,
        &scope.parts.machines.machines(),
        &scope.default_machine,
    );
    step.declare(scope, source);
    let active: Vec<_> = active
        .iter()
        .map(|message| tool_references(message, wired.mode, &prompt.tool_names))
        .collect();
    let mut shape = model_shape(selection, capabilities, &scope.parts.limits);
    // 摘要请求不写缓存（context.md §3 第 7 条）。
    shape.normalize.skip_cache_write = *source == QuerySource::Compact;
    let max_tokens = shape.max_tokens;
    let request = ModelRequest {
        selection: selection.clone(),
        system,
        messages: normalize(active.iter().map(AsRef::as_ref), &shape.normalize),
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
    Prepared {
        request,
        max_tokens,
    }
}
