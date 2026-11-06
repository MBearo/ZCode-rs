//! 驾驶舱在入口的部分（docs/specs/rust-cli/tui-cockpit.md §1）：`<cockpit>/<id>.turn.json`（owner 写）与 `<id>.seen.json`（看的人
//! 写）两个笔记，两个写者不抢同一个文件，每个文件原子写；登记处与笔记的监听；`tmux switch-client`。
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use futures_util::stream::{self, BoxStream, StreamExt};
use rust_cli_fs::{FileMode, ParentDir, TempName, WriteOptions};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::peers::TmuxPlace;
use rust_cli_paths::StoragePaths;
use rust_cli_tui::services::{CockpitNote, CockpitPort};
use serde_json::{Value, json};

/// 临时名是隐藏的：监听滤掉它（`json_changes`）。
const WRITE: WriteOptions = WriteOptions {
    mode: FileMode::Default,
    temp_name: TempName::Hidden,
    parent: ParentDir::Create,
    rename_retry: &[],
};
/// 一次读笔记的并发。
const NOTES_AT_ONCE: usize = 16;

pub(crate) struct LocalCockpit {
    dir: PathBuf,
    peers: PathBuf,
    tmux_socket: Option<String>,
}

impl LocalCockpit {
    pub(crate) fn new(paths: &StoragePaths, env: &rust_cli_base::secret::EnvSnapshot) -> Self {
        Self {
            dir: paths.cockpit().as_path().to_path_buf(),
            peers: paths.peers().as_path().to_path_buf(),
            tmux_socket: crate::assemble::peers::tmux_place(env).map(|place| place.socket),
        }
    }

    fn turn_path(&self, id: &SessionId) -> PathBuf {
        self.dir.join(format!("{}.turn.json", id.as_str()))
    }

    fn seen_path(&self, id: &SessionId) -> PathBuf {
        self.dir.join(format!("{}.seen.json", id.as_str()))
    }
}

async fn read_json(path: &std::path::Path) -> Option<Value> {
    let bytes = rust_cli_fs::read_optional(path).await.ok()??;
    serde_json::from_slice(&bytes).ok()
}

async fn write_json(path: PathBuf, value: Value) {
    let bytes = value.to_string().into_bytes();
    if let Err(error) = rust_cli_fs::atomic_write(&path, &bytes, WRITE).await {
        tracing::warn!(%error, path = %path.display(), "cannot write the cockpit note");
    }
}

impl CockpitPort for LocalCockpit {
    fn changes(&self) -> BoxStream<'static, ()> {
        stream::select(
            rust_cli_env_local::json_changes(self.dir.clone()),
            rust_cli_env_local::json_changes(self.peers.clone()),
        )
        .boxed()
    }

    fn notes(&self, ids: Vec<SessionId>) -> BoxFuture<'static, HashMap<SessionId, CockpitNote>> {
        let paths: Vec<_> = ids
            .into_iter()
            .map(|id| (self.turn_path(&id), self.seen_path(&id), id))
            .collect();
        async move {
            stream::iter(paths)
                .map(|(turn, seen, id)| async move {
                    let turn = read_json(&turn).await.unwrap_or_default();
                    let seen = read_json(&seen).await.unwrap_or_default();
                    let note = CockpitNote {
                        ended_at_ms: turn["endedAt"].as_u64(),
                        failed: turn["failed"].as_str().map(str::to_owned),
                        seen_at_ms: seen["seenAt"].as_u64(),
                    };
                    (id, note)
                })
                .buffer_unordered(NOTES_AT_ONCE)
                .filter(|(_, note)| std::future::ready(*note != CockpitNote::default()))
                .collect()
                .await
        }
        .boxed()
    }

    fn mark_seen(&self, id: SessionId, at_ms: u64) -> BoxFuture<'static, ()> {
        let path = self.seen_path(&id);
        async move { write_json(path, json!({ "seenAt": at_ms })).await }.boxed()
    }

    fn write_turn(
        &self,
        id: SessionId,
        ended_at_ms: u64,
        failed: Option<String>,
    ) -> BoxFuture<'static, ()> {
        let path = self.turn_path(&id);
        async move {
            let note = match failed {
                Some(failed) => json!({ "endedAt": ended_at_ms, "failed": failed }),
                None => json!({ "endedAt": ended_at_ms }),
            };
            write_json(path, note).await;
        }
        .boxed()
    }

    fn forget(&self, id: SessionId) -> BoxFuture<'static, ()> {
        let (turn, seen) = (self.turn_path(&id), self.seen_path(&id));
        async move {
            let _ = rust_cli_fs::remove_optional(&turn).await;
            let _ = rust_cli_fs::remove_optional(&seen).await;
        }
        .boxed()
    }

    fn go_to(&self, place: TmuxPlace) -> BoxFuture<'static, Result<(), String>> {
        async move {
            let output = rust_cli_proc::command("tmux")
                .args(["-S", &place.socket, "switch-client", "-t", &place.pane])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .output()
                .await
                .map_err(|error| error.to_string())?;
            if output.status.success() {
                Ok(())
            } else {
                Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
            }
        }
        .boxed()
    }

    fn tmux_socket(&self) -> Option<String> {
        self.tmux_socket.clone()
    }
}

#[cfg(test)]
#[path = "tui_cockpit_tests.rs"]
mod tests;
