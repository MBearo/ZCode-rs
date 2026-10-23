//! 设备身份 deviceMid（docs/specs/rust-cli/platform.md §10，工作线 F52，用户决定 2026-10-22：“device_id 计算可以看一下
//! 之前 nodejs 的逻辑”）。
//!
//! 照 Node CLI 的 `ensureCliDeviceMid`（`apps/zcode-cli/packages/adapters/src/device/cli-device-mid.ts`）：与 App、远端
//! zcode-server、Node CLI 共用 `<dataBaseDir>/.zcode/v2/telemetry-state.json` 的 `deviceMid`。先不持锁读；没有时在
//! `telemetry-state.lock` 里再读一次，还没有才生成并原子写（保留文件里的其他字段）。出错不挡请求：用本进程生成的
//! 那一个 UUID。进程内只解析一次。

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use rust_cli_fs::{ParentDir, PidLockOptions, TempName, WriteOptions};
use rust_cli_paths::StoragePaths;
use serde_json::{Map, Value};
use tokio::sync::OnceCell;

/// 生成新 UUID（入口给 `NodeIds::uuid`：小写带连字符的 v4，与 Node `createUuid()` 同一格式）。
pub type NewUuid = Arc<dyn Fn() -> String + Send + Sync>;

/// 设备身份（每个进程一份）。
pub struct DeviceMid {
    state: PathBuf,
    lock: PathBuf,
    uuid: NewUuid,
    lock_options: PidLockOptions,
    /// 本进程生成的 UUID：要写进文件的值，也是出错时的后备（只生成一次，Node `getGeneratedDeviceMid`）。
    generated: OnceLock<String>,
    resolved: OnceCell<String>,
}

impl DeviceMid {
    pub fn new(paths: &StoragePaths, uuid: NewUuid) -> Self {
        Self {
            state: paths.telemetry_state().as_path().to_path_buf(),
            lock: paths.telemetry_lock().as_path().to_path_buf(),
            uuid,
            lock_options: PidLockOptions::default(),
            generated: OnceLock::new(),
            resolved: OnceCell::new(),
        }
    }

    /// 测试用：缩短锁的等待。
    #[cfg(test)]
    pub(crate) fn with_lock_options(mut self, options: PidLockOptions) -> Self {
        self.lock_options = options;
        self
    }

    /// 设备身份。第一次调用读文件（必要时持锁生成并写入），之后返回同一个值；并发的第一次调用等同一次解析。
    pub async fn get(&self) -> String {
        self.resolved
            .get_or_init(|| async {
                match self.ensure().await {
                    Ok(value) => value,
                    Err(error) => {
                        tracing::warn!(
                            event = "device_mid.fallback",
                            error = %error,
                            "device id is not persisted; this process uses a generated one"
                        );
                        self.generated().to_owned()
                    }
                }
            })
            .await
            .clone()
    }

    fn generated(&self) -> &str {
        self.generated.get_or_init(|| (self.uuid)())
    }

    /// Node `ensurePersistedDeviceMid`。
    async fn ensure(&self) -> io::Result<String> {
        if let Some(value) = device_mid(&read_state(&self.state).await) {
            return Ok(value);
        }
        rust_cli_fs::with_pid_lock(&self.lock, &self.lock_options, async {
            let mut state = read_state(&self.state).await;
            // 别的进程在我们等锁时写好了：用它，不写。
            if let Some(value) = device_mid(&state) {
                return Ok(value);
            }
            let value = self.generated().to_owned();
            state.insert("deviceMid".to_owned(), Value::String(value.clone()));
            let text = crate::cli_config::node_pretty(&Value::Object(state));
            rust_cli_fs::atomic_write(&self.state, text.as_bytes(), WRITE).await?;
            Ok(value)
        })
        .await
    }
}

/// Node `writeTelemetryState`：同目录 `.telemetry-state.json.<pid>.<ms>.<rand>.tmp` 再 rename，目录不存在时创建。
const WRITE: WriteOptions = WriteOptions {
    mode: rust_cli_fs::FileMode::Default,
    temp_name: TempName::Hidden,
    parent: ParentDir::Create,
    rename_retry: &[],
};

/// Node `readTelemetryState`：读不出、不是 JSON、顶层不是对象时是空对象。
async fn read_state(path: &std::path::Path) -> Map<String, Value> {
    let Ok(Some(bytes)) = rust_cli_fs::read_optional(path).await else {
        return Map::new();
    };
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(Value::Object(state)) => state,
        _ => Map::new(),
    }
}

/// Node `readDeviceMid`：非空字符串。
fn device_mid(state: &Map<String, Value>) -> Option<String> {
    state
        .get("deviceMid")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
#[path = "device_tests.rs"]
mod tests;
