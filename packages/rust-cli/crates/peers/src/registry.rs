//! 登记处（docs/specs/rust-cli/cross-session.md §2.2）：`<peers>/<pid>.json`，0600，原子写；读的一方清掉 pid 已不存在
//! 的记录与它的 socket（清扫）。IO 只经 `rust-cli-fs`。

use std::path::{Path, PathBuf};

use rust_cli_kernel_api::peers::{PeerAddress, PeerRecord, PeerStatus};

use crate::frames::Record;

pub(crate) fn record_path(dir: &Path, pid: u32) -> PathBuf {
    dir.join(format!("{pid}.json"))
}

const OPTIONS: rust_cli_fs::WriteOptions = rust_cli_fs::WriteOptions {
    mode: rust_cli_fs::FileMode::Private,
    temp_name: rust_cli_fs::TempName::Hidden,
    parent: rust_cli_fs::ParentDir::Existing,
    rename_retry: &[],
};

pub(crate) async fn write(dir: &Path, record: &Record) -> std::io::Result<()> {
    let text = serde_json::to_vec(record).unwrap_or_default();
    rust_cli_fs::atomic_write(&record_path(dir, record.pid), &text, OPTIONS).await
}

pub(crate) async fn remove(dir: &Path, pid: u32, socket: &Path) {
    for path in [record_path(dir, pid), socket.to_path_buf()] {
        if let Err(error) = rust_cli_fs::remove_optional(&path).await {
            tracing::debug!(%error, path = %path.display(), "peer registry cleanup failed");
        }
    }
}

fn status(text: Option<&str>) -> Option<PeerStatus> {
    match text {
        Some("busy") => Some(PeerStatus::Busy),
        Some("idle") => Some(PeerStatus::Idle),
        _ => None,
    }
}

/// 活着的同伴（不含 `own`），按 pid 文件名排序；pid 已不存在的记录与 socket 删掉。
pub(crate) async fn list(dir: &Path, own: u32) -> Vec<PeerRecord> {
    let names = match rust_cli_fs::file_names(dir).await {
        Ok(names) => names,
        Err(error) => {
            tracing::debug!(%error, "the peer registry could not be read");
            return Vec::new();
        }
    };
    let mut peers = Vec::new();
    for name in names {
        let Some(pid) = name
            .strip_suffix(".json")
            .and_then(|stem| stem.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == own {
            continue;
        }
        let path = dir.join(&name);
        let Ok(Some(bytes)) = rust_cli_fs::read_optional(&path).await else {
            continue;
        };
        let Ok(record) = serde_json::from_slice::<Record>(&bytes) else {
            continue;
        };
        if record.pid != pid {
            continue;
        }
        if !rust_cli_fs::process_alive(pid) {
            remove(dir, pid, Path::new(&record.messaging_socket_path)).await;
            continue;
        }
        peers.push(PeerRecord {
            pid,
            name: record.name,
            address: PeerAddress::from_socket(&record.messaging_socket_path),
            kind: record.kind,
            status: status(record.status.as_deref()),
            started_at_ms: record.started_at,
        });
    }
    peers
}
