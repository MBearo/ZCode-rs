//! rewind（docs/specs/rust-cli/session-lifecycle.md §5）：对话回退是一次存储提交；代码回退是
//! 执行环境的 token 事务，在后台工作里跑，结果回到 actor 后才提交事实、发事件、回复命令。

use rust_cli_kernel_api::env::{OperationContext, Origin, WorkspaceRef};
use rust_cli_kernel_api::env_rewind::{
    RestoreAction, RestoreFile, RestoredFile, RewindPlan, RewindPrepare, RewindStatus,
};
use rust_cli_kernel_api::ids::{CommandId, MessageId, OperationId, OwnerEpoch};
use rust_cli_kernel_api::machine::Checkpoints;
use rust_cli_kernel_api::session_api::{Ack, KernelEvent, RewindOption, RewindScope};
use rust_cli_kernel_api::store::{ArtifactStore, Fact, RevertCursor, WorkspaceCheckpoint};
use serde_json::{Value, json};
use tokio::sync::oneshot;

use crate::actor::{Actor, Job, JobDone, rejected};
use crate::rewind_plan::{self, Artifact};

/// 选择器的快照：可选的 user 消息与每一回合的 checkpoint（读 artifact 在调用方做）。
pub(crate) struct Snapshot {
    options: Vec<(MessageId, Vec<WorkspaceCheckpoint>, bool)>,
}

/// 代码回退事务的结果，回到 actor 后提交。
pub(crate) struct Finished {
    command: CommandId,
    target: MessageId,
    scope: RewindScope,
    reply: oneshot::Sender<Ack>,
    token: OperationId,
    checkpoints: Vec<String>,
    result: Result<Value, String>,
}

async fn read(
    store: &dyn ArtifactStore,
    checkpoint: &WorkspaceCheckpoint,
) -> Result<Artifact, String> {
    let bytes = store
        .get(&checkpoint.snapshot)
        .await
        .map_err(|error| error.to_string())?;
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

/// 每条可选消息这一回合的改动（列表每行的摘要）；artifact 读不出的回合按没有改动显示。
pub(crate) async fn options(store: &dyn ArtifactStore, snapshot: Snapshot) -> Vec<RewindOption> {
    let mut out = Vec::new();
    for (message, turn, code) in snapshot.options {
        let mut artifacts = Vec::new();
        for checkpoint in &turn {
            if let Ok(artifact) = read(store, checkpoint).await {
                artifacts.push(artifact);
            }
        }
        out.push(RewindOption {
            message,
            files: rewind_plan::changes(&artifacts),
            code,
        });
    }
    out
}

/// 执行环境的回退事务（spec §5.2）：prepare（逐个核对 hash）→ apply（写前再核对，失败已补偿）。token 是
/// `context.operation`，调用方在提交事实之后 `rewind_finish`。
async fn restore(
    checkpoints: &dyn Checkpoints,
    workspace: &WorkspaceRef,
    context: OperationContext,
    files: Vec<RestoreFile>,
) -> Result<Vec<RestoredFile>, String> {
    let token = context.operation.clone();
    let plan = checkpoints
        .rewind_prepare(workspace, context, RewindPrepare { files })
        .await
        .map_err(|error| error.to_string())?;
    if let RewindPlan::Conflicts { files } = plan {
        let lines: Vec<String> = files
            .iter()
            .map(|f| rewind_plan::conflict(f.path.as_str()))
            .collect();
        return Err(lines.join("\n"));
    }
    match checkpoints
        .rewind_apply(&token)
        .await
        .map_err(|e| e.to_string())?
    {
        RewindStatus::Applied { files } => Ok(files),
        RewindStatus::Failed { message, conflicts } if !conflicts.is_empty() => {
            let lines: Vec<String> = conflicts
                .iter()
                .map(|f| rewind_plan::conflict(f.path.as_str()))
                .collect();
            Err(format!("{message}\n{}", lines.join("\n")))
        }
        RewindStatus::Failed { message, .. } => Err(message),
        other => Err(format!("unexpected rewind status: {other:?}")),
    }
}

/// 回执里的文件：`[{path, action}]`。
fn restored_json(files: &[RestoredFile]) -> Value {
    json!(
        files
            .iter()
            .map(|f| json!({"path": f.path, "action": match f.action {
                RestoreAction::Restore => "restore",
                RestoreAction::Delete => "delete",
            }}))
            .collect::<Vec<_>>()
    )
}

impl Actor {
    pub(crate) fn rewind_snapshot(&self) -> Snapshot {
        let options = self
            .messages
            .iter()
            .filter_map(rewind_plan::selectable)
            .map(|user| {
                let turn = self
                    .checkpoints
                    .iter()
                    .filter(|c| c.prompt == user.id)
                    .cloned()
                    .collect();
                let code =
                    !rewind_plan::checkpoints_from(&self.messages, &self.checkpoints, &user.id)
                        .is_empty();
                (user.id.clone(), turn, code)
            })
            .collect();
        Snapshot { options }
    }

    pub(crate) async fn rewind(
        &mut self,
        command: CommandId,
        target: MessageId,
        scope: RewindScope,
        reply: oneshot::Sender<Ack>,
    ) {
        if self.busy() {
            let _ = reply.send(rejected("a turn is running; rewind after it finishes"));
            return;
        }
        if !self
            .messages
            .iter()
            .any(|m| m.id() == &target && rewind_plan::selectable(m).is_some())
        {
            let _ = reply.send(rejected(format!("{target} is not a message to rewind to")));
            return;
        }
        let undo: Vec<WorkspaceCheckpoint> =
            rewind_plan::checkpoints_from(&self.messages, &self.checkpoints, &target)
                .into_iter()
                .cloned()
                .collect();
        let code = matches!(scope, RewindScope::Code | RewindScope::Both);
        if !code || (undo.is_empty() && scope == RewindScope::Both) {
            let ack = match self
                .rewind_conversation(Some(command), &target, Vec::new())
                .await
            {
                Ok(removed) => Ack::Accepted {
                    result: json!({"restored": [], "removed": removed}),
                },
                Err(reason) => rejected(reason),
            };
            let _ = reply.send(ack);
            return;
        }
        if undo.is_empty() {
            let _ = reply.send(rejected("there are no code changes to restore"));
            return;
        }
        let token = self.deps.ids.operation();
        let context = OperationContext {
            environment: self.meta.workspace.environment.clone(),
            generation: self.deps.generation.clone(),
            owner_epoch: OwnerEpoch::LOCAL,
            operation: token.clone(),
            origin: Origin::User {
                session: Some(self.meta.id.clone()),
            },
        };
        let workspace = self.meta.workspace.clone();
        let (port, store) = (
            self.deps.machine.checkpoints().clone(),
            self.deps.artifacts.clone(),
        );
        let checkpoints = undo.iter().map(|c| c.id.clone()).collect();
        let task = self.deps.spawner.spawn("rewind-code", async move {
            let result = async {
                let mut artifacts = Vec::new();
                for checkpoint in &undo {
                    artifacts.push(read(store.as_ref(), checkpoint).await?);
                }
                let files = rewind_plan::plan(&artifacts)?;
                let restored = restore(port.as_ref(), &workspace, context, files).await?;
                Ok(restored_json(&restored))
            }
            .await;
            JobDone::Rewind(Box::new(Finished {
                command,
                target,
                scope,
                reply,
                token,
                checkpoints,
                result,
            }))
        });
        self.job = Some(Job { task, cancel: None });
    }

    /// 事务结束：先提交（撤销的 checkpoint、对话游标），再改内存、发事件、回复，最后释放 token。
    pub(crate) async fn finish_rewind(&mut self, finished: Finished) {
        let Finished {
            command,
            target,
            scope,
            reply,
            token,
            checkpoints,
            result,
        } = finished;
        let ack = match result {
            Err(reason) => rejected(reason),
            Ok(restored) => {
                let fact = Fact::CheckpointsRestored {
                    checkpoints: checkpoints.clone(),
                };
                let committed = if scope == RewindScope::Both {
                    self.rewind_conversation(Some(command), &target, vec![fact])
                        .await
                } else {
                    self.commit(Some(command), vec![fact]).await.map(|()| 0)
                };
                match committed {
                    Ok(removed) => {
                        self.checkpoints.retain(|c| !checkpoints.contains(&c.id));
                        Ack::Accepted {
                            result: json!({"restored": restored, "removed": removed}),
                        }
                    }
                    Err(reason) => rejected(reason),
                }
            }
        };
        let _ = reply.send(ack);
        let checkpoints = self.deps.machine.checkpoints().clone();
        drop(self.deps.spawner.spawn("rewind-finish", async move {
            if let Err(error) = checkpoints.rewind_finish(&token).await {
                tracing::warn!(%error, "rewind token was not released");
            }
        }));
        self.start_next().await;
    }

    /// esc 撤回刚发的输入（`Command::RestoreCancelled`，修复线 F38，spec §5.5）。判断与回退在
    /// actor 的一次处理里：TUI 判断时内核可能已经开了排队的回合或通知的回合，这里看到的才是唯一的事实。回执带回
    /// 这条输入（前端放回输入框；`HistoryRewritten` 之后的视图里已经没有它）。
    pub(crate) async fn restore_cancelled(&mut self, command: CommandId) -> Ack {
        if self.busy() {
            return rejected("a turn is running");
        }
        if !self.queue.is_empty() {
            return rejected("inputs are queued");
        }
        let Some(index) = self
            .messages
            .iter()
            .rposition(|m| rewind_plan::selectable(m).is_some())
        else {
            return rejected("there is no input to restore");
        };
        if rewind_plan::answered(&self.messages[index + 1..]) {
            return rejected("the input was answered");
        }
        let message = self.messages[index].clone();
        match self
            .rewind_conversation(Some(command), message.id(), Vec::new())
            .await
        {
            Ok(removed) => Ack::Accepted {
                result: json!({"message": message, "removed": removed}),
            },
            Err(reason) => rejected(reason),
        }
    }

    /// 历史截到 target 之前（`session.revert` 单游标）；`extra` 与游标在同一批提交。返回去掉的消息数。
    async fn rewind_conversation(
        &mut self,
        command: Option<CommandId>,
        target: &MessageId,
        extra: Vec<Fact>,
    ) -> Result<usize, String> {
        let index = self
            .messages
            .iter()
            .position(|m| m.id() == target)
            .ok_or_else(|| format!("{target} is not in the conversation"))?;
        let cut_after = self
            .messages
            .last()
            .map(|m| m.id().clone())
            .unwrap_or_else(|| target.clone());
        let generation = self.generation + 1;
        let cursor = RevertCursor {
            target: target.clone(),
            kept: self.messages[..index]
                .iter()
                .map(|m| m.id().clone())
                .collect(),
            cut_after,
            generation,
        };
        let mut facts = extra;
        facts.push(Fact::RevertSet { cursor });
        self.commit(command, facts).await?;
        let removed = self.messages.len() - index;
        self.messages.truncate(index);
        self.generation = generation;
        self.log
            .append(KernelEvent::HistoryRewritten { generation });
        Ok(removed)
    }
}

#[cfg(test)]
#[path = "rewind_tests.rs"]
pub(crate) mod tests;

#[cfg(test)]
#[path = "restore_cancelled_tests.rs"]
mod restore_cancelled_tests;
