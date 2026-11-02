//! 回合里待回答的交互：登记、回答与取消（从 turn.rs 拆出）。

use super::*;

impl Actor {
    /// 待回答的交互：登记在内存里并通知前端；回答经 `ResolveInteraction` 命令送达。
    ///
    /// 不落库（D4，工作线 F130）：以前提问与回答各写一条 `rust-cli/pending_interaction` / `interaction_resolved`
    /// entry，冷读取读出 `LoadedSession.pending_interactions`，却没有读者——进程重启后询问已无法回答，续写时未结的
    /// 工具调用按中断结算（session-lifecycle.md §6.3）。现在两头一起删；库里的旧记录不读、不删。
    pub(crate) fn ask(
        &mut self,
        request: InteractionRequest,
        reply: oneshot::Sender<Result<InteractionAnswer, OrchestrationError>>,
        scope: crate::actor::AskScope,
    ) {
        let id = self.deps.ids.interaction();
        let (kind, tool) = describe(&request);
        tracing::info!(event = "interaction.asked", session = %self.meta.id, interaction = %id, kind, tool, "interaction asked");
        self.log.append(KernelEvent::InteractionAsked {
            interaction: id.clone(),
            request: request.clone(),
        });
        let _notify = self.notify_permission(&request);
        self.pending.push(Pending {
            id,
            request,
            reply,
            _notify,
            scope,
        });
        self.asking_changed();
    }

    pub(crate) async fn resolve(
        &mut self,
        command: CommandId,
        interaction: InteractionId,
        answer: Answer,
    ) -> Ack {
        let Some(position) = self
            .pending
            .iter()
            .position(|pending| pending.id == interaction)
        else {
            return rejected(format!("no pending interaction {interaction}"));
        };
        // “总是允许”的规则先落库，成功了才算回答（shell-permissions.md §5）；没有要记住的规则时不写库。
        if let Some(rules) = crate::rules::remembered(&self.pending[position].request, &answer)
            && let Err(reason) = self.commit(Some(command), vec![rules]).await
        {
            return rejected(reason);
        }
        let pending = self.pending.remove(position);
        self.asking_changed();
        // 只记决定与记住的规则数，不记回答里的文字（logging.md §4）。
        let (kind, tool) = describe(&pending.request);
        tracing::info!(
            event = "interaction.resolved",
            session = %self.meta.id,
            interaction = %interaction,
            kind,
            tool,
            decision = answer.decision(),
            remembered = answer.remembered().len(),
            waiting = !pending.reply.is_closed(),
            "interaction resolved"
        );
        let _ = pending.reply.send(Ok(InteractionAnswer {
            interaction: interaction.clone(),
            answer,
        }));
        self.log
            .append(KernelEvent::InteractionResolved { interaction });
        accepted()
    }

    /// 关闭会话或进程退出：全部待回答的交互取消，等待方得到错误。
    pub(crate) fn cancel_pending(&mut self) {
        let pending = std::mem::take(&mut self.pending);
        self.asking_changed();
        for pending in pending {
            self.cancelled(pending);
        }
    }

    /// 回合结束或被中断：只取消回合里的询问，会话级的询问（后台子代理的，agents.md §4.1.1）留着。
    ///
    /// 修复线 F67：以前回合结束时取消全部待回答的交互（那时只有回合里的询问）。
    pub(crate) fn cancel_turn_asks(&mut self) {
        let (turn, session): (Vec<_>, Vec<_>) = std::mem::take(&mut self.pending)
            .into_iter()
            .partition(|pending| pending.scope == crate::actor::AskScope::Turn);
        self.pending = session;
        self.asking_changed();
        for pending in turn {
            self.cancelled(pending);
        }
    }

    /// 工具被取消，撤回它的询问（`Interactions::cancel_ask`）。
    ///
    /// 修复（R1-5）：以前工具被取消时只丢掉等回答的 future，这里的待回答交互要到回合结束才清——对话框一直显示，
    /// 用户之后的回答还会被接受、提交“总是允许”。现在工具取消时发来这条消息：从待回答里去掉、记为取消、通知前端
    /// 关闭对话框；之后到达的回答找不到它，被拒绝（`resolve`），不提交规则。
    pub(crate) fn cancel_ask(&mut self, tool_use: &rust_cli_kernel_api::ids::ToolUseId) {
        let Some(position) = self
            .pending
            .iter()
            .position(|pending| pending.request.tool_use() == tool_use)
        else {
            return;
        };
        let pending = self.pending.remove(position);
        self.asking_changed();
        self.cancelled(pending);
    }

    /// 一条交互取消并通知前端（等待方随 `reply` 丢弃得到错误）。
    fn cancelled(&self, pending: Pending) {
        tracing::info!(event = "interaction.cancelled", session = %self.meta.id, interaction = %pending.id, "interaction cancelled");
        self.log.append(KernelEvent::InteractionResolved {
            interaction: pending.id,
        });
    }
}

/// 日志里的交互种类与工具名（不含输入与文字）。
fn describe(request: &InteractionRequest) -> (&'static str, &str) {
    match request {
        InteractionRequest::Permission { tool, .. } => ("permission", tool.as_str()),
        InteractionRequest::Question { .. } => ("question", ""),
        InteractionRequest::PlanApproval { .. } => ("plan_approval", ""),
    }
}
