//! 子代理的信号（docs/specs/rust-cli/agents.md §5）：后台子代理的终态通知与任务清单变化。
//!
//! 与后台命令的通知（[`crate::notices`]）同一个模式：actor 直接持有信号流，在同一个 `select!` 里处理，
//! 所以输入、后台命令通知与子代理通知只有一个 owner。两种通知进同一条 next 通道（[`crate::lane`]），
//! 按到达顺序在工具边界并入或开回合；提交之后才 ack（至少一次，重复的按对话里同文本的消息去掉）。

use rust_cli_kernel_api::agents::{AgentNotice, AgentSignal};
use rust_cli_kernel_api::ids::ShellTaskId;
use rust_cli_kernel_api::session_api::KernelEvent;

use crate::actor::Actor;
use crate::lane::Waiting;

/// 子代理的信号流（流结束后不再等它）。
pub(crate) type AgentStream = crate::watched::Watched<AgentSignal>;

impl Actor {
    pub(crate) async fn on_agent_signal(&mut self, signal: AgentSignal) {
        match signal {
            AgentSignal::TasksChanged { tasks } => {
                self.log.append(KernelEvent::TasksChanged { tasks });
            }
            AgentSignal::Notice(notice) => self.on_agent_notice(notice).await,
        }
    }

    async fn on_agent_notice(&mut self, notice: AgentNotice) {
        // 关闭中不收（R1-1）：不 ack，通知留在 agents 里，会话再打开时送达。
        if self.closing.is_some() {
            return;
        }
        let work = notice.agent.to_string();
        let status = notice.status.clone();
        if !self.enqueue_notice(Waiting::Agent(notice)).await {
            return;
        }
        self.log
            .append(KernelEvent::BackgroundWork { work, status });
        if self.running.is_none() {
            self.start_next().await;
        }
    }

    pub(crate) async fn ack_agent(&self, seq: u64) {
        self.deps.agents.ack(&self.meta.id, seq).await;
    }

    /// 还在跑或通知还没 ack 的后台子代理。与后台命令放在同一栏（后台任务是同一类）。
    pub(crate) async fn background_agents(&self) -> Vec<ShellTaskId> {
        self.deps
            .agents
            .background(&self.meta.id)
            .await
            .into_iter()
            .map(|agent| ShellTaskId::new(agent.as_str()))
            .collect()
    }
}
