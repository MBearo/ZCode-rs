//! 子代理的组装（docs/specs/rust-cli/agents.md §8）：子循环工厂与 agents 能力的接线。

use std::sync::Arc;

use rust_cli_kernel_api::agents::SubagentQueries;
use rust_cli_kernel_api::orchestration::{ContextBuilder, QueryRunner, ToolRegistry};
use rust_cli_query::{QueryLoop, QueryParts};

/// 子循环工厂：主循环的其余能力不变，换上子代理的工具集与上下文。
pub(crate) struct LoopFactory {
    pub(crate) parts: QueryParts,
}

impl SubagentQueries for LoopFactory {
    fn runner(
        &self,
        tools: Arc<dyn ToolRegistry>,
        context: Arc<dyn ContextBuilder>,
    ) -> Arc<dyn QueryRunner> {
        Arc::new(QueryLoop::new(QueryParts {
            tools,
            context,
            ..self.parts.clone()
        }))
    }
}

/// 后台命令通知的状态与信封，交给 agents（它不依赖 query，agents.md §5.2）。
pub(crate) fn shell_notice(
    notice: &rust_cli_kernel_api::env_shell::ShellNotice,
) -> (&'static str, String) {
    (
        rust_cli_query::task_status(notice),
        rust_cli_query::task_envelope(notice),
    )
}
