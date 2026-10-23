//! 执行状态的修改（docs/specs/rust-cli/tools-more.md §2.3、shell-permissions.md §4.6）。执行状态（权限模式 ×
//! plan 开关）只有会话 actor 一个 owner：前端的 `SetMode` / `SetPlan` 命令与工具的 `Interactions::set_mode` /
//! `set_plan`（EnterPlanMode、ExitPlanMode）走同一个入口，先提交 `ExecutionChanged` 事实，成功后才改内存里的
//! 状态并通知前端。主循环经 `RunSink::current_mode` 读判决用的模式（plan 开时是 plan），所以同一回合里后续的
//! 工具调用立即按新状态判决。
//!
//! 修复线 F20（用户决定）：plan 是独立开关。以前 plan 是一个模式，进入前的模式不在这里（TUI 记 `before_plan`，
//! ExitPlanMode 按审批的回答换模式）；现在权限模式一直在 actor 里，开关 plan 不动它，退出 plan 后就是进入前的值。
//!
//! 计划模式的两个会话标志也在这里（docs/specs/rust-cli/context.md §13.4）：改状态成功后按判决用的（旧, 新）
//! 模式更新，提交 `plan_mode_reentry` / `plan_mode_exit` 后清掉对应的标志。

use rust_cli_kernel_api::ids::CommandId;
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::permission::{ExecutionState, PermissionMode};
use rust_cli_kernel_api::session_api::KernelEvent;
use rust_cli_kernel_api::store::Fact;

use crate::actor::Actor;

impl Actor {
    /// 判决、工具、上下文与 hook 看的模式：plan 开时是 plan，否则是权限模式。
    pub(crate) fn mode(&self) -> PermissionMode {
        self.execution.effective()
    }

    /// 工具改权限模式（`Interactions::set_mode`；`Plan` 是旧写法，只打开 plan）。
    pub(crate) async fn change_mode(
        &mut self,
        command: Option<CommandId>,
        mode: PermissionMode,
    ) -> Result<(), String> {
        self.apply(command, self.execution.with_mode(mode), false)
            .await
    }

    /// 工具开关 plan（EnterPlanMode、ExitPlanMode）：权限模式不变，不写项目的持久模式。
    pub(crate) async fn change_plan(&mut self, enabled: bool) -> Result<(), String> {
        self.apply(None, self.execution.with_plan(enabled), false)
            .await
    }

    /// 用户切换权限模式（前端的 `SetMode`，shift+tab 与 `/mode`）：同一批写本机项目的持久模式
    /// （docs/specs/rust-cli/config.md §2.2，Node `setMode` 写 `runtime.getMode()`，即切换后的权限模式；`plan`
    /// 只打开 plan，写的仍是当前的权限模式）。plan 开关不变（Node 的 `setMode` 会关掉 plan，见 spec 的待确认）。
    pub(crate) async fn user_mode(
        &mut self,
        command: CommandId,
        mode: PermissionMode,
    ) -> Result<(), String> {
        self.apply(Some(command), self.execution.with_mode(mode), true)
            .await
    }

    /// 用户开关 plan（前端的 `SetPlan`，`/plan`）：同 App 的 plan 开关（Node `setExecutionState({planEnabled})`），
    /// 不写项目的持久模式。
    pub(crate) async fn user_plan(
        &mut self,
        command: CommandId,
        enabled: bool,
    ) -> Result<(), String> {
        self.apply(Some(command), self.execution.with_plan(enabled), false)
            .await
    }

    async fn apply(
        &mut self,
        command: Option<CommandId>,
        next: ExecutionState,
        save: bool,
    ) -> Result<(), String> {
        let mut facts = vec![Fact::ExecutionChanged { state: next }];
        if save {
            facts.push(Fact::ProjectModeSet { mode: next.mode });
        }
        self.commit(command, facts).await?;
        let previous = self.execution;
        self.plan_flags = self
            .plan_flags
            .after_transition(previous.effective(), next.effective());
        self.execution = next;
        if previous.mode != next.mode {
            self.log
                .append(KernelEvent::ModeChanged { mode: next.mode });
            // 模式类别可能变了：扣住的同伴消息重新判断（cross-session.md §1.6）。
            self.peers_mode_changed(true).await;
        }
        if previous.plan != next.plan {
            self.log
                .append(KernelEvent::PlanChanged { enabled: next.plan });
        }
        Ok(())
    }

    /// 已提交的消息若是 context 产生的 reentry / exit，清掉对应的标志。
    pub(crate) fn plan_attachment_committed(&mut self, message: &Message) {
        if let Message::Attachment(attachment) = message {
            self.plan_flags = self.plan_flags.after_commit(&attachment.source);
        }
    }
}

#[cfg(test)]
#[path = "mode_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "plan_flags_tests.rs"]
mod plan_flags_tests;
