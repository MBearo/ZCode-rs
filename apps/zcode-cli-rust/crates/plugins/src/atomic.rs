//! Crash recovery of atomically replaced plugin storage paths, shared with the
//! Node runtime (`adapters/src/plugins/atomic-directory.ts`): a writer keeps
//! `.<name>.backup` and `.<name>.transaction.json` beside the target while it
//! replaces it. Readers recover a dead writer's leftovers and, while the writer
//! is alive, read the generation its authority file has committed.
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

/// This process's writer identity in transaction records.
pub static OWNER_ID: LazyLock<String> = LazyLock::new(|| uuid::Uuid::new_v4().to_string());

/// In-process writers keyed by target path (M10.4 registers them).
pub static ACTIVE: LazyLock<Mutex<std::collections::HashMap<PathBuf, Live>>> =
    LazyLock::new(Default::default);

#[derive(Clone, Debug)]
pub struct Live {
    pub authority: Option<PathBuf>,
    pub had_target: bool,
    pub coordinated: bool,
    pub transaction_id: String,
}

pub fn sidecars(target: &Path) -> (PathBuf, PathBuf) {
    let parent = target.parent().unwrap_or(Path::new(""));
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    (
        parent.join(format!(".{name}.backup")),
        parent.join(format!(".{name}.transaction.json")),
    )
}

enum Record {
    V1 {
        stage: String,
    },
    V2 {
        stage: String,
        live: Live,
        owner_id: String,
        owner_pid: i64,
    },
}

async fn read_record(path: &Path) -> Option<Record> {
    let value = crate::fsx::read_json_lenient(path).await?;
    let stage = value["stageName"].as_str()?.to_owned();
    match value["version"].as_i64()? {
        1 => Some(Record::V1 { stage }),
        2 => {
            let pid = value["ownerPid"].as_i64().filter(|p| *p > 0)?;
            let mode = value["mode"].as_str()?;
            if mode != "coordinated" && mode != "standalone" {
                return None;
            }
            let authority = match &value["authorityPath"] {
                Value::Null => None,
                Value::String(path) => Some(PathBuf::from(path)),
                _ => return None,
            };
            Some(Record::V2 {
                stage,
                owner_id: value["ownerId"].as_str()?.to_owned(),
                owner_pid: pid,
                live: Live {
                    authority,
                    had_target: value["hadTarget"].as_bool()?,
                    coordinated: mode == "coordinated",
                    transaction_id: value["transactionId"].as_str()?.to_owned(),
                },
            })
        }
        _ => None,
    }
}

/// Node `isAtomicTransactionOwnerAlive`.
fn owner_alive(owner_id: &str, pid: i64) -> bool {
    if pid == std::process::id() as i64 {
        return owner_id == OWNER_ID.as_str();
    }
    process_alive(pid)
}

#[cfg(unix)]
fn process_alive(pid: i64) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // 与 Node process.kill(pid, 0) 相同：成功或 EPERM 都表示进程存在。
    let sent = unsafe { libc::kill(pid, 0) } == 0;
    sent || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
fn process_alive(pid: i64) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    let Ok(pid) = u32::try_from(pid) else {
        return false;
    };
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            // 无权打开同样说明进程存在（Node 的 EPERM 语义）。
            return std::io::Error::last_os_error().raw_os_error() == Some(5);
        }
        let mut code = 0;
        let alive = GetExitCodeProcess(handle, &mut code) != 0 && code == STILL_ACTIVE as u32;
        CloseHandle(handle);
        alive
    }
}

/// Node `authorityContainsTransactionSync`: any object in the authority JSON
/// carries `cacheTransactionId == id`.
async fn authority_contains(authority: &Path, id: &str) -> bool {
    let Ok(path) = Box::pin(recover(authority)).await else {
        return false;
    };
    let Some(value) = crate::fsx::read_json_lenient(&path).await else {
        return false;
    };
    let mut pending = vec![&value];
    while let Some(current) = pending.pop() {
        match current {
            Value::Object(map) => {
                if map.get("cacheTransactionId").and_then(Value::as_str) == Some(id) {
                    return true;
                }
                pending.extend(map.values());
            }
            Value::Array(items) => pending.extend(items),
            _ => {}
        }
    }
    false
}

/// Node `resolveLiveAtomicReadPath`: the committed generation of a live write.
async fn live_read_path(target: &Path, backup: &Path, live: &Live) -> PathBuf {
    if live.coordinated
        && let Some(authority) = &live.authority
        && authority_contains(authority, &live.transaction_id).await
    {
        return target.to_owned();
    }
    if crate::fsx::exists(backup).await {
        return backup.to_owned();
    }
    if live.had_target && crate::fsx::exists(target).await {
        return target.to_owned();
    }
    backup.to_owned()
}

async fn remove_all(path: &Path) -> std::io::Result<()> {
    match tokio::fs::symlink_metadata(path).await {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
        Ok(m) if m.is_dir() => tokio::fs::remove_dir_all(path).await,
        Ok(_) => tokio::fs::remove_file(path).await,
    }
}

/// Node `recoverAtomicTargetSync`: the path to read for `target`.
pub async fn recover(target: &Path) -> std::io::Result<PathBuf> {
    let (backup, transaction_path) = sidecars(target);
    let key = crate::fsx::normalize(&std::path::absolute(target)?);
    let active = ACTIVE.lock().expect("atomic registry").get(&key).cloned();
    if let Some(live) = active {
        return Ok(live_read_path(target, &backup, &live).await);
    }
    let record = read_record(&transaction_path).await;
    if let Some(Record::V2 {
        live,
        owner_id,
        owner_pid,
        ..
    }) = &record
        && owner_alive(owner_id, *owner_pid)
    {
        return Ok(live_read_path(target, &backup, live).await);
    }
    let (has_target, has_backup) = (
        crate::fsx::exists(target).await,
        crate::fsx::exists(&backup).await,
    );
    match &record {
        Some(Record::V2 { live, .. }) if live.coordinated => {
            let committed = match &live.authority {
                Some(authority) => authority_contains(authority, &live.transaction_id).await,
                None => false,
            };
            if committed {
                if has_target && has_backup {
                    remove_all(&backup).await?;
                } else if !has_target && has_backup {
                    // 权威状态只会在 target 激活后写入；异常磁盘状态下优先恢复一个完整版本。
                    tokio::fs::rename(&backup, target).await?;
                }
            } else if has_backup {
                remove_all(target).await?;
                tokio::fs::rename(&backup, target).await?;
            } else if !live.had_target {
                remove_all(target).await?;
            }
        }
        _ if !has_target && has_backup => tokio::fs::rename(&backup, target).await?,
        _ if has_target && has_backup => remove_all(&backup).await?,
        _ => {}
    }
    let stage = match &record {
        Some(Record::V1 { stage } | Record::V2 { stage, .. }) => Some(stage),
        None => None,
    };
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if let Some(stage) = stage.filter(|s| s.starts_with(&format!(".{name}.stage-"))) {
        remove_all(&target.parent().unwrap_or(Path::new("")).join(stage)).await?;
    }
    match tokio::fs::remove_file(&transaction_path).await {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    Ok(target.to_owned())
}

#[cfg(test)]
#[path = "atomic_tests.rs"]
mod tests;
