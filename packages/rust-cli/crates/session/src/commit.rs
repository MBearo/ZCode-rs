//! 会话 actor 的一批提交（docs/specs/rust-cli/session-lifecycle.md §2）：当前轮由 actor 的 `RunLedger` 给出，续写时
//! 补在内存里、还没落库的消息排在第一批带消息的提交最前面（§6.3、§6.3.1）。先落库，调用方在成功之后才改内存、发事件。

use rust_cli_kernel_api::ids::{CommandId, MessageId, OwnerEpoch};
use rust_cli_kernel_api::session_api::Ack;
use rust_cli_kernel_api::store::{Fact, FactBatch, StoreError, TurnAnchor};

use crate::actor::{Actor, accepted, rejected};

impl Actor {
    /// 提交一批（先落库，再改内存、发事件：调用方在成功之后才改状态）。当前轮由 actor 的 `RunLedger` 给出（性能修复，
    /// 修复线 F6，审查 R5-3：存储不再为 assistant 与每个用量行各从库里推导一次），提交成功后才记下。
    ///
    /// 失败原样给出存储的错误（工作线 F130，审查 C15）：以前转成字符串，`NotFound` / `StaleOwner` 与别的失败分不开，
    /// 回合失败时再包一层 `StoreError::Storage` 交给主循环（文字成了 `storage error: storage error: …`）。
    pub(crate) async fn commit(
        &mut self,
        command: Option<CommandId>,
        facts: Vec<Fact>,
    ) -> Result<(), StoreError> {
        let pending = self.pending_resume(&facts);
        let stored_pending = !pending.is_empty();
        let facts = pending.into_iter().chain(facts).collect();
        let batch = self.ledger.batch(command, facts, self.deps.ids.as_ref());
        let turn = batch.turn.clone();
        self.deps.store.commit(&self.meta.id, batch).await?;
        self.ledger.committed(turn);
        if stored_pending {
            self.resume_pending.clear();
        }
        Ok(())
    }

    /// 续写时补的消息还没落库、这一批有消息：返回它们，按历史里的顺序排在这一批最前面（工作线 F77、F81，
    /// session-lifecycle.md §6.3、§6.3.1）。已不在活动历史里（被回退掉）的丢掉。
    fn pending_resume(&mut self, facts: &[Fact]) -> Vec<Fact> {
        if !facts
            .iter()
            .any(|fact| matches!(fact, Fact::MessageAppended { .. }))
        {
            return Vec::new();
        }
        let found = self.resume_facts(&self.resume_pending);
        if found.is_empty() {
            self.resume_pending.clear();
        }
        found
    }

    /// 续写时补的消息里还在活动历史里的那些（被回退掉的丢掉）。
    fn resume_facts(&self, pending: &[MessageId]) -> Vec<Fact> {
        pending
            .iter()
            .filter_map(|id| self.messages.iter().find(|message| message.id() == id))
            .map(|message| Fact::MessageAppended {
                message: message.clone(),
            })
            .collect()
    }

    /// 会话里切到这个会话（`/resume`、`/fork`）之后：续写时补在内存里、还没落库的消息（末尾未结调用的中断结果与
    /// `No response requested.`）马上单独提交一批（工作线 F79、F81，session-lifecycle.md §6.3、§6.3.1）。没有要写的时候
    /// 什么都不做；提交失败时放回去，随下一批带消息的提交写。
    pub(crate) async fn store_resume_sentinel(&mut self, id: CommandId) -> Ack {
        // 先取下再提交：`commit` 看到这一批有消息时不会再把它们排进来一次。
        let pending = std::mem::take(&mut self.resume_pending);
        let facts = self.resume_facts(&pending);
        if facts.is_empty() {
            return accepted();
        }
        match self.commit(Some(id), facts).await {
            Ok(()) => accepted(),
            Err(reason) => {
                self.resume_pending = pending;
                rejected(reason)
            }
        }
    }

    /// 提交一批锚在给定回合的事实，不改当前轮：起任务时记下回合、结果晚到的旁路请求用（会话标题的用量行，
    /// usage.md §2.1.1，工作线 F73）。
    pub(crate) async fn commit_in_turn(
        &self,
        turn: Option<TurnAnchor>,
        facts: Vec<Fact>,
    ) -> Result<(), StoreError> {
        let batch = FactBatch {
            owner_epoch: OwnerEpoch::LOCAL,
            command: None,
            facts,
            turn,
        };
        self.deps
            .store
            .commit(&self.meta.id, batch)
            .await
            .map(|_| ())
    }

    pub(crate) fn turn_anchor(&self) -> Option<TurnAnchor> {
        self.ledger.turn().cloned()
    }
}
