//! 主循环（docs/specs/rust-cli/README.md §3）。
//!
//! 包括 transition 状态机、流式工具执行器（tool_use 块
//! 结束就开工、按输入判定并发、结果顺序、Bash 出错不取消其余调用、按中止原因回填）、恢复分支、
//! 中途输入、发给模型前的消息整理。
//!
//! 只依赖 kernel-api 的端口与编排 trait；实现由 runtime 注入。消息、操作意图都经 [`RunSink`]
//! 提交，提交成功后才继续（提交屏障）。
//!
//! 已实现：文本回路、工具执行器与中断语义。恢复分支、自动压缩、Stop hooks、中途输入属于
//! 工作线 L1 的后续步骤。

mod assembler;
mod compaction;
mod defaults;
mod deferred;
mod executor;
mod goal;
mod limits;
mod machines;
#[cfg(test)]
mod mcp_support_tests;
mod mcp_wait;
mod normalize;
mod output_budget;
mod pairing;
mod recovery;
mod request;
mod respond;
mod run;
mod step;
mod stop_hooks;
mod texts;

pub use deferred::step_tool_search;
pub use limits::{RequestLimits, ThinkingTokens};

/// `/context` 的消息估算（docs/specs/rust-cli/context-usage.md §3 i）：与主循环相同的规范化（对话中间的
/// system 消息按生效配置保留或降级，`configured` 答不出时按模型卡片，model.md §11），不带每轮 effort。
///
/// 修改（工作线 F93，用户决定 2026-10-24）：`/context` 只用本地估算、不发 count_tokens，F89 为计数请求加的 ToolSearch
/// 参数（历史里的 `tool_reference` 先经主请求的降级函数，端点才认）随之删掉。
pub fn api_messages<'a>(
    messages: impl IntoIterator<Item = &'a rust_cli_kernel_api::message::Message>,
    model: &str,
    configured: Option<bool>,
) -> Vec<rust_cli_kernel_api::model::ApiMessage> {
    let options = normalize::NormalizeOptions {
        system_turns: rust_cli_prompts::models::mid_conversation_system(model, configured),
        turn_effort: None,
        // 缓存断点与主循环相同（摘要请求才前移一条，compact.md）。
        skip_cache_write: false,
    };
    normalize::normalize(messages, &options)
}

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::clock::Ids;
use rust_cli_kernel_api::env::Operations;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::env_registry::EnvironmentRegistry;
use rust_cli_kernel_api::ids::{AgentId, RunId, SessionId};
use rust_cli_kernel_api::model::ModelPort;
use rust_cli_kernel_api::orchestration::{
    Compactor, ContextBuilder, HookRunner, QueryError, QueryRequest, QueryRunner, QueryStop,
    RunSink, RunView, ToolExecutor, ToolRegistry,
};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::store::ArtifactStore;

/// 主循环用到的能力，由 runtime 注入。
#[derive(Clone)]
pub struct QueryParts {
    pub model: Arc<dyn ModelPort>,
    pub context: Arc<dyn ContextBuilder>,
    pub tools: Arc<dyn ToolRegistry>,
    pub executor: Arc<dyn ToolExecutor>,
    pub compactor: Arc<dyn Compactor>,
    pub hooks: Arc<dyn HookRunner>,
    pub ids: Arc<dyn Ids>,
    /// 模型请求与工具调用的计时（docs/specs/rust-cli/usage.md §1）：计时只在这里读时钟。
    pub clock: Arc<dyn rust_cli_kernel_api::clock::Clock>,
    /// 执行环境的注册表（R3，remote-exec.md §20.5、§20.6）：工具调用缺省在它的主 env 上执行（按调用的 `machine` 换，
    /// §15.8.1），controller 的服务（WebFetch）也从它取；连上的远端机器决定工具定义里的 `machine` 参数；子代理的缺省机器
    /// 从这里取。工作线 F131：以前另存一份主 env 与 controller 的服务，与注册表给的是同一份。
    pub machines: Arc<dyn EnvironmentRegistry>,
    pub artifacts: Arc<dyn ArtifactStore>,
    /// 操作上下文的工厂：执行环境的当前实例（本地是进程 uuid）与 `ids`（F134：以前另带一个 `generation`）。
    pub operations: Operations,
    /// 非交互会话（`-p`）：主线程的断流续写只在这时进行。
    pub non_interactive: bool,
    /// 入口按环境变量给的输出上限与 thinking 开关（query.md §1.5，修复线 F42）。
    pub limits: RequestLimits,
}

/// 一次运行内不变的上下文。
pub(crate) struct RunScope {
    pub(crate) session: SessionId,
    pub(crate) run: RunId,
    pub(crate) workspace: WorkspaceRef,
    pub(crate) agent: Option<AgentId>,
    pub(crate) mode: PermissionMode,
    /// 这次运行的缺省机器（主 env 或子代理派生时的机器；R3，remote-exec.md §20.6）：取自请求，一次运行里不变。
    pub(crate) default_machine: rust_cli_kernel_api::ids::MachineName,
    pub(crate) abort: Abort,
    pub(crate) sink: Arc<dyn RunSink>,
    pub(crate) parts: QueryParts,
    /// 本次运行里等满过预算的 MCP 服务器（直接调用连接中服务器的工具，ext.md §3.8）。
    pub(crate) exhausted: mcp_wait::Exhausted,
}

impl RunScope {
    /// 一次运行的上下文：会话、运行、工作区、子代理、模式与缺省机器取自请求。
    pub(crate) fn new(
        request: &QueryRequest,
        sink: Arc<dyn RunSink>,
        parts: QueryParts,
        abort: Abort,
    ) -> Self {
        Self {
            session: request.session.clone(),
            run: request.run.clone(),
            workspace: request.workspace.clone(),
            agent: request.agent.clone(),
            mode: request.mode,
            default_machine: request.default_machine.clone(),
            abort,
            sink,
            parts,
            exhausted: Default::default(),
        }
    }

    /// 会话现在的状态：输出端的一次读取（同一时刻的值，工作线 F134）。会话 actor 是唯一 owner，工具改过的模式与工作目录
    /// 在同一回合里立即生效（docs/specs/rust-cli/tools-more.md §2.3、§11.4）；输出端不持有会话的状态时用请求里的模式与
    /// 工作区、缺省的计划模式标志，没有主模型与目标（context.md §13.4）。
    pub(crate) async fn view(&self) -> RunView {
        self.sink
            .view()
            .await
            .unwrap_or_else(|| RunView::new(self.mode, self.workspace.clone()))
    }
}

pub struct QueryLoop {
    parts: QueryParts,
}

impl QueryLoop {
    pub fn new(parts: QueryParts) -> Self {
        Self { parts }
    }

    pub fn parts(&self) -> &QueryParts {
        &self.parts
    }
}

#[async_trait]
impl QueryRunner for QueryLoop {
    async fn run(
        &self,
        request: QueryRequest,
        sink: Arc<dyn RunSink>,
        abort: Abort,
    ) -> Result<QueryStop, QueryError> {
        let scope = Arc::new(RunScope::new(&request, sink, self.parts.clone(), abort));
        run::run(scope, request).await
    }
}
