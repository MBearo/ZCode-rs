//! Node 的 `/fork [latest|checkpointId]`（docs/specs/rust-cli/session-lifecycle.md §6.5）：从 checkpoint 分出新会话。
//!
//! checkpoint 只从 actor 的清单里选（唯一来源）。恢复文件时先跑执行环境的回退事务（§6.5.3，在 actor 起的后台
//! 工作里，不持有锁跨 await），文件恢复成功后才由存储一次写完新会话（`SessionStore::fork`）；`ForkFiles::Keep` 直接写。

use rust_cli_kernel_api::env::{EnvPath, OperationContext, Origin};
use rust_cli_kernel_api::env_rewind::{RestoreFile, RestoredFile, content_hash};
use rust_cli_kernel_api::ids::{OperationId, OwnerEpoch};
use rust_cli_kernel_api::session_api::{Ack, ForkFiles};
use rust_cli_kernel_api::store::{SessionFork, WorkspaceCheckpoint};
use serde_json::json;
use tokio::sync::oneshot;

use crate::actor::{Actor, Job, JobDone, rejected};
use crate::rewind::{read, restore, restored_json};
use crate::rewind_plan::Artifact;

/// 文件恢复的结果，回到 actor 后写新会话（冲突的说明与代码回退相同：`Cannot restore code: …`）。
pub(crate) struct Restored {
    reply: oneshot::Sender<Ack>,
    checkpoint: WorkspaceCheckpoint,
    token: OperationId,
    result: Result<Vec<RestoredFile>, String>,
}

/// 恢复计划（§6.5.3）：文件集合与恢复内容按 Node（所选 checkpoint 写过的文件、写入前的内容，写入前不存在就
/// 删除）；期望的磁盘内容是这个路径在所选 checkpoint 及之后最后一次写入后的内容。`artifacts[0]` 是所选的。
pub(crate) fn plan(artifacts: &[Artifact]) -> Result<Vec<RestoreFile>, String> {
    let Some((selected, later)) = artifacts.split_first() else {
        return Ok(Vec::new());
    };
    let mut out: Vec<RestoreFile> = Vec::new();
    for file in &selected.files {
        if out.iter().any(|planned| planned.path.as_str() == file.path) {
            continue;
        }
        let last = later
            .iter()
            .rev()
            .flat_map(|artifact| artifact.files.iter().rev())
            .find(|written| written.path == file.path)
            .unwrap_or(file);
        let Some(after) = &last.after_content else {
            return Err(format!("the checkpoint of {} has no content", file.path));
        };
        let restore = file
            .before_content
            .clone()
            .filter(|_| file.existed_before)
            .map(String::into_bytes);
        out.push(RestoreFile {
            path: EnvPath::new(file.path.clone()),
            expected: Some(content_hash(after.as_bytes())),
            restore,
        });
    }
    Ok(out)
}

fn files_word(count: usize) -> &'static str {
    if count == 1 { "file" } else { "files" }
}

impl Actor {
    /// `ForkCheckpoint`：回执要等文件恢复与记录都完成（与代码回退相同）。
    pub(crate) async fn fork_checkpoint(
        &mut self,
        checkpoint: Option<String>,
        files: ForkFiles,
        reply: oneshot::Sender<Ack>,
    ) {
        if self.busy() {
            let _ = reply.send(rejected("a turn is running; fork after it finishes"));
            return;
        }
        let index = match &checkpoint {
            Some(id) => self.checkpoints.iter().position(|c| &c.id == id),
            None => self.checkpoints.len().checked_sub(1),
        };
        let Some(index) = index else {
            // Node `forkWorkspaceFromCheckpoint` 的原文。
            let reason = match checkpoint {
                Some(id) => format!("Checkpoint not found: {id}"),
                None => "No workspace checkpoint is available yet.".to_owned(),
            };
            let _ = reply.send(rejected(reason));
            return;
        };
        let selected = self.checkpoints[index].clone();
        if files == ForkFiles::Keep {
            let ack = self.write_fork(&selected, None).await;
            let _ = reply.send(ack);
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
        let involved: Vec<WorkspaceCheckpoint> = self.checkpoints[index..].to_vec();
        let workspace = self.meta.workspace.clone();
        let (env, store) = (self.deps.env.clone(), self.deps.artifacts.clone());
        let task = self.deps.spawner.spawn("fork-restore", async move {
            let result = async {
                let mut artifacts = Vec::new();
                for checkpoint in &involved {
                    artifacts.push(read(store.as_ref(), checkpoint).await?);
                }
                restore(env.as_ref(), &workspace, context, plan(&artifacts)?).await
            }
            .await;
            JobDone::Fork(Box::new(Restored {
                reply,
                checkpoint: selected,
                token,
                result,
            }))
        });
        self.job = Some(Job { task, cancel: None });
    }

    /// 文件恢复结束：成功时写新会话；最后释放 token（记录先于释放，与代码回退相同）。
    pub(crate) async fn finish_fork(&mut self, restored: Restored) {
        let Restored {
            reply,
            checkpoint,
            token,
            result,
        } = restored;
        let ack = match result {
            Err(reason) => rejected(reason),
            Ok(files) => self.write_fork(&checkpoint, Some(files)).await,
        };
        let _ = reply.send(ack);
        let env = self.deps.env.clone();
        drop(self.deps.spawner.spawn("fork-restore-finish", async move {
            if let Err(error) = env.rewind_finish(&token).await {
                tracing::warn!(%error, "fork restore token was not released");
            }
        }));
        self.start_next().await;
    }

    /// 存储一次写完新会话（§6.5.2）；`restored` 有值时是恢复了文件的 checkpoint 分叉。
    async fn write_fork(
        &self,
        checkpoint: &WorkspaceCheckpoint,
        restored: Option<Vec<RestoredFile>>,
    ) -> Ack {
        let child = self.deps.ids.session();
        let fork = SessionFork {
            parent: self.meta.id.clone(),
            child: child.clone(),
            created_at_ms: self.deps.clock.now_ms(),
            through: checkpoint.prompt.clone(),
            checkpoint: restored.as_ref().map(|_| checkpoint.clone()),
            restored: restored.clone().unwrap_or_default(),
            selection: self.selection.clone(),
            mode: self.execution.mode,
            plan: self.execution.plan,
        };
        let receipt = match self.deps.store.fork(fork).await {
            Ok(receipt) => receipt,
            Err(error) => {
                if restored.is_some() {
                    tracing::warn!(%error, "files were restored but the forked session was not written");
                }
                return rejected(error.to_string());
            }
        };
        let response = match &restored {
            Some(files) => format!(
                "Forked session {child} from checkpoint {}: copied {} messages and restored {} {}.",
                checkpoint.id,
                receipt.copied,
                files.len(),
                files_word(files.len())
            ),
            None => format!(
                "Forked session {child} from message {}: copied {} messages.",
                checkpoint.prompt, receipt.copied
            ),
        };
        Ack::Accepted {
            result: json!({
                "session": child.as_str(),
                "response": response,
                "copied": receipt.copied,
                "restored": restored_json(&restored.unwrap_or_default()),
            }),
        }
    }
}

#[cfg(test)]
#[path = "fork_checkpoint_tests.rs"]
mod tests;
