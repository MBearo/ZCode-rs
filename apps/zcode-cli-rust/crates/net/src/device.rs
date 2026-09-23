//! Device id shared with Desktop through `telemetry-state.json`
//! (Node `adapters/src/device/cli-device-mid.ts`). Any failure falls back to an
//! id that is valid for this process only; the caller caches the result.
use serde_json::{Map, Value};
use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const LOCK_RETRY_DELAY: Duration = Duration::from_millis(10);
const LOCK_RETRY_COUNT: usize = 200;
const LOCK_STALE: Duration = Duration::from_secs(5 * 60);

/// `${ZCODE_DATA_BASE_DIR || home}/.zcode/v2/telemetry-state.json`.
pub fn state_file(base_dir: Option<&str>, home: &Path, cwd: &Path) -> PathBuf {
    let base = base_dir.map(str::trim).filter(|b| !b.is_empty());
    let base = match base {
        None | Some("~") => home.to_path_buf(),
        Some(path) => match path.strip_prefix("~/") {
            Some(rest) => home.join(rest),
            None => cwd.join(path),
        },
    };
    base.join(".zcode").join("v2").join("telemetry-state.json")
}

/// Node `ensureCliDeviceMid` without the process cache.
pub async fn ensure(state_file: &Path) -> String {
    let generated = uuid::Uuid::new_v4().to_string();
    match persisted(state_file, &generated).await {
        Ok(id) => id,
        Err(error) => {
            tracing::warn!(
                target: "zcode::net",
                event = "device.persist_failed",
                error = %error,
                "Device id is not persisted; using a process-local id"
            );
            generated
        }
    }
}

fn device_id(state: &Map<String, Value>) -> Option<String> {
    state
        .get("deviceMid")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

async fn read_state(path: &Path) -> Map<String, Value> {
    match tokio::fs::read(path)
        .await
        .map(|b| serde_json::from_slice(&b))
    {
        Ok(Ok(Value::Object(state))) => state,
        _ => Map::new(),
    }
}

async fn persisted(state_file: &Path, generated: &str) -> std::io::Result<String> {
    if let Some(id) = device_id(&read_state(state_file).await) {
        return Ok(id);
    }
    let directory = state_file.parent().unwrap_or(Path::new("."));
    tokio::fs::create_dir_all(directory).await?;
    let lock = directory.join("telemetry-state.lock");
    for _ in 0..LOCK_RETRY_COUNT {
        match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
            .await
        {
            Ok(mut handle) => {
                let result = locked(state_file, &mut handle, generated).await;
                drop(handle);
                let _ = tokio::fs::remove_file(&lock).await;
                return result;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if !remove_stale_lock(&lock).await {
                    tokio::time::sleep(LOCK_RETRY_DELAY).await;
                }
            }
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::other("CLI telemetry state lock timeout"))
}

async fn locked(
    state_file: &Path,
    handle: &mut tokio::fs::File,
    generated: &str,
) -> std::io::Result<String> {
    use tokio::io::AsyncWriteExt;
    let owner = serde_json::json!({"createdAt": now_ms(), "pid": std::process::id()});
    handle.write_all(owner.to_string().as_bytes()).await?;
    let mut state = read_state(state_file).await;
    if let Some(id) = device_id(&state) {
        return Ok(id);
    }
    state.insert("deviceMid".into(), generated.into());
    write_state(state_file, &state).await?;
    Ok(generated.to_owned())
}

/// Atomic replace so Desktop never reads a torn file.
async fn write_state(path: &Path, state: &Map<String, Value>) -> std::io::Result<()> {
    let directory = path.parent().unwrap_or(Path::new("."));
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("state");
    let temp = directory.join(format!(
        ".{name}.{}.{}.{}.tmp",
        std::process::id(),
        now_ms(),
        uuid::Uuid::new_v4().simple()
    ));
    let body = serde_json::to_vec_pretty(state).map_err(std::io::Error::other)?;
    let result = async {
        tokio::fs::write(&temp, body).await?;
        tokio::fs::rename(&temp, path).await
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temp).await;
    }
    result
}

/// Node `removeStaleTelemetryLockIfNeeded`.
async fn remove_stale_lock(lock: &Path) -> bool {
    let Ok(metadata) = tokio::fs::metadata(lock).await else {
        return false;
    };
    let age = metadata
        .modified()
        .ok()
        .and_then(|m| SystemTime::now().duration_since(m).ok())
        .unwrap_or_default();
    if age < LOCK_STALE {
        let owner = tokio::fs::read(lock)
            .await
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .filter(|o| o["createdAt"].is_number())
            .and_then(|o| o["pid"].as_f64());
        match owner {
            None => return false,
            Some(pid) if process_alive(pid) => return false,
            Some(_) => {}
        }
    }
    let _ = tokio::fs::remove_file(lock).await;
    true
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Node `isProcessAlive` (`process.kill(pid, 0)`; EPERM counts as alive).
fn process_alive(pid: f64) -> bool {
    if pid.fract() != 0.0 || pid <= 0.0 || pid > f64::from(u32::MAX) {
        return false;
    }
    alive(pid as u32)
}

#[cfg(unix)]
fn alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: 信号 0 只做存在性与权限检查，不向进程发送信号。
    let delivered = unsafe { libc::kill(pid, 0) } == 0;
    delivered || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
fn alive(pid: u32) -> bool {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, ERROR_ACCESS_DENIED, GetLastError, STILL_ACTIVE},
        System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    };
    // SAFETY: 句柄只在本函数内使用并关闭；与 libuv `uv_kill(pid, 0)` 的判定一致。
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return GetLastError() == ERROR_ACCESS_DENIED;
        }
        let mut code = 0u32;
        let running = GetExitCodeProcess(handle, &mut code) != 0 && code == STILL_ACTIVE as u32;
        CloseHandle(handle);
        running
    }
}

#[cfg(not(any(unix, windows)))]
fn alive(_pid: u32) -> bool {
    true
}
