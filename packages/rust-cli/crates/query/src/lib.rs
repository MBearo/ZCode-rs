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
#[cfg(test)]
mod mcp_support_tests;
mod mcp_wait;
mod normalize;
mod notification;
mod pairing;
mod recovery;
mod respond;
mod run;
mod step;
mod stop_hooks;
mod texts;

pub use deferred::step_tool_search;
pub use limits::{RequestLimits, ThinkingTokens};
pub use notification::{
    monitor_envelope, monitor_notice_text, task_envelope, task_notification, task_status,
};

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
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::ids::{AgentId, Generation, RunId, SessionId};
use rust_cli_kernel_api::local_services::LocalServices;
use rust_cli_kernel_api::machine::MachineEnv;
use rust_cli_kernel_api::model::ModelPort;
use rust_cli_kernel_api::orchestration::{
    Compactor, ContextBuilder, HookRunner, QueryError, QueryRequest, QueryRunner, QueryStop,
    RunSink, ToolExecutor, ToolRegistry,
};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::plan::PlanModeFlags;
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
    /// 工具调用执行所在的机器（R0 只有本机；R3 起按调用的 `machine` 换，remote-exec.md §15.8.1）。
    pub machine: MachineEnv,
    /// 只在本机的服务（Git、worktree、WebFetch）。
    pub local: LocalServices,
    pub artifacts: Arc<dyn ArtifactStore>,
    /// 执行环境的当前实例（本地是进程 uuid）。
    pub generation: Generation,
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
    pub(crate) abort: Abort,
    pub(crate) sink: Arc<dyn RunSink>,
    pub(crate) parts: QueryParts,
    /// 本次运行里等满过预算的 MCP 服务器（直接调用连接中服务器的工具，ext.md §3.8）。
    pub(crate) exhausted: mcp_wait::Exhausted,
}

impl RunScope {
    /// 会话现在的权限模式：会话 actor 是唯一 owner，工具（EnterPlanMode / ExitPlanMode）改过后同一回合
    /// 立即生效；输出端不持有模式时用请求里的值（docs/specs/rust-cli/tools-more.md §2.3）。
    pub(crate) async fn current_mode(&self) -> PermissionMode {
        self.sink.current_mode().await.unwrap_or(self.mode)
    }

    /// 组装上下文用的模式与计划模式的会话标志：一次取回 actor 的同一个快照；输出端不持有时用请求里的
    /// 模式与默认标志（docs/specs/rust-cli/context.md §13.4）。
    pub(crate) async fn current_mode_and_plan_flags(&self) -> (PermissionMode, PlanModeFlags) {
        self.sink
            .current_mode_and_plan_flags()
            .await
            .unwrap_or((self.mode, PlanModeFlags::default()))
    }

    /// 会话现在的工作目录：EnterWorktree / ExitWorktree 改过后同一回合立即生效；输出端不持有时用
    /// 请求里的值（docs/specs/rust-cli/tools-more.md §11.4）。
    pub(crate) async fn current_workspace(&self) -> WorkspaceRef {
        self.sink
            .current_workspace()
            .await
            .unwrap_or_else(|| self.workspace.clone())
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
        let scope = Arc::new(RunScope {
            session: request.session.clone(),
            run: request.run.clone(),
            workspace: request.workspace.clone(),
            agent: request.agent.clone(),
            mode: request.mode,
            abort,
            sink,
            parts: self.parts.clone(),
            exhausted: Default::default(),
        });
        run::run(scope, request).await
    }
}
