//! 文件 checkpoint 进入会话清单（docs/specs/rust-cli/session-lifecycle.md §4）。
//!
//! 配对（`ToolFinished` 时记下、工具结果提交时同批落库，连同子代理关联）在 kernel-api 的 `RunLedger` 里，子代理的
//! sink 用同一份（工作线 F130）；这里只做提交成功之后的那一步。

use rust_cli_kernel_api::session_api::{FileChange, KernelEvent};
use rust_cli_kernel_api::store::WorkspaceCheckpoint;

use crate::actor::Actor;

impl Actor {
    /// 提交成功后：进入清单，发 `FileChanges`。
    pub(crate) fn checkpoints_settled(&mut self, settled: Vec<(WorkspaceCheckpoint, FileChange)>) {
        let Some(turn) = self.running.as_ref().map(|r| r.turn.clone()) else {
            return;
        };
        if settled.is_empty() {
            return;
        }
        let mut files = Vec::new();
        for (checkpoint, change) in settled {
            self.checkpoints.push(checkpoint);
            files.push(change);
        }
        self.log.append(KernelEvent::FileChanges {
            turn,
            files,
            rewindable: true,
        });
    }
}
