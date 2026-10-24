//! 跨机器的代码回退（暂定，docs/specs/rust-cli/remote-exec.md §20.7.5）：要撤销的 checkpoint 按机器分组，每台机器一个回退
//! 事务。先让所有机器都 prepare 通过再 apply；任一台 prepare 失败（读不出、冲突、没连上）就整体拒绝（token 照常释放）；
//! apply 中途失败时报告每台机器的结果，成功的那几台不补偿。只有本机时与原来相同（一个事务、原来的文字）。

use std::sync::Arc;

use rust_cli_kernel_api::env::{OperationContext, WorkspaceRef};
use rust_cli_kernel_api::env_rewind::{
    RestoreFile, RestoredFile, RewindPlan, RewindPrepare, RewindStatus,
};
use rust_cli_kernel_api::ids::OperationId;
use rust_cli_kernel_api::machine::Checkpoints;
use rust_cli_kernel_api::store::{ArtifactStore, WorkspaceCheckpoint};

use crate::rewind_plan::{self, Artifact};

/// 一台机器上的回退：checkpoint、端口、工作区与 token。
pub(crate) struct Group {
    /// 远端机器名；本机为 None。
    pub machine: Option<String>,
    pub checkpoints: Vec<WorkspaceCheckpoint>,
    pub port: Arc<dyn Checkpoints>,
    pub workspace: WorkspaceRef,
    pub context: OperationContext,
}

impl Group {
    pub(crate) fn token(&self) -> &OperationId {
        &self.context.operation
    }

    /// 结果文字：只有本机时原样；否则前面写机器名（本机写 `local`）。
    fn label(&self, message: &str, many: bool) -> String {
        match (&self.machine, many) {
            (None, false) => message.to_owned(),
            (None, true) => format!("local: {message}"),
            (Some(name), _) => format!("{name}: {message}"),
        }
    }

    fn ids(&self) -> Vec<String> {
        self.checkpoints.iter().map(|c| c.id.clone()).collect()
    }
}

/// 一台机器上恢复了的文件（远端的带机器名）。
pub(crate) type Restored = (Option<String>, Vec<RestoredFile>);

/// 回退的结果：撤销成功的 checkpoint，与回执（全部成功）或拒绝的理由。
pub(crate) struct Outcome {
    pub restored: Vec<String>,
    pub result: Result<Vec<Restored>, String>,
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

fn conflicts(files: &[rust_cli_kernel_api::env_rewind::RewindConflict]) -> String {
    files
        .iter()
        .map(|f| rewind_plan::conflict(f.path.as_str()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 一台机器的 prepare：读 artifact、算要恢复的文件、登记 token。
async fn prepare(store: &dyn ArtifactStore, group: &Group) -> Result<(), String> {
    let mut artifacts = Vec::new();
    for checkpoint in &group.checkpoints {
        artifacts.push(read(store, checkpoint).await?);
    }
    let files: Vec<RestoreFile> = rewind_plan::plan(&artifacts)?;
    let plan = group
        .port
        .rewind_prepare(
            &group.workspace,
            group.context.clone(),
            RewindPrepare { files },
        )
        .await
        .map_err(|error| error.to_string())?;
    match plan {
        RewindPlan::Conflicts { files } => Err(conflicts(&files)),
        _ => Ok(()),
    }
}

async fn apply(group: &Group) -> Result<Vec<RestoredFile>, String> {
    match group
        .port
        .rewind_apply(group.token())
        .await
        .map_err(|e| e.to_string())?
    {
        RewindStatus::Applied { files } => Ok(files),
        RewindStatus::Failed {
            message,
            conflicts: found,
        } if !found.is_empty() => Err(format!("{message}\n{}", conflicts(&found))),
        RewindStatus::Failed { message, .. } => Err(message),
        other => Err(format!("unexpected rewind status: {other:?}")),
    }
}

/// 全部 prepare 再全部 apply。每台机器的 token 由调用方在回复之后统一 `rewind_finish`（没 prepare 过的释放是空操作）。
pub(crate) async fn restore(store: &dyn ArtifactStore, groups: &[Group]) -> Outcome {
    let many = groups.len() > 1;
    for group in groups {
        if let Err(message) = prepare(store, group).await {
            return Outcome {
                restored: Vec::new(),
                result: Err(group.label(&message, many)),
            };
        }
    }
    let mut restored = Vec::new();
    let mut done = Vec::new();
    let mut lines = Vec::new();
    let mut failed = false;
    for group in groups {
        match apply(group).await {
            Ok(files) => {
                restored.extend(group.ids());
                lines.push(group.label("restored", many));
                done.push((group.machine.clone(), files));
            }
            Err(message) => {
                failed = true;
                lines.push(group.label(&message, many));
            }
        }
    }
    let result = if !failed {
        Ok(done)
    } else if groups.len() == 1 {
        // 只有一台机器：原来的文字。
        Err(lines.pop().unwrap_or_default())
    } else {
        Err(lines.join("\n"))
    };
    Outcome { restored, result }
}
