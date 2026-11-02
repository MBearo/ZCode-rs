// 读取语义复制自 apps/zcode-host/crates/settings/src/settings/mod.rs（1f0bfd1b71）的 `get`：文件不存在或
// 不是对象时按空设置处理。改动：只读不写（写入、迁移与 schema 补全仍由 App 负责），文件经 rust-cli-fs 读取，
// 解析失败不重试（Host 为并发写入重试 3 次；CLI 读到半截文件时按空设置处理，下次读取再看）。
//! App 设置 `~/.zcode/v2/setting.json` 的只读视图（docs/specs/rust-cli/platform.md §6）。App 的连接选择不读
//! （工作线 F49：所有可用的套餐平铺进目录，不跟随 App 的当前连接，§9）。

use rust_cli_paths::StoragePath;
use serde_json::{Map, Value};

/// 设置文件的读取方。每次读取都以磁盘为准，不缓存。
#[derive(Debug, Clone)]
pub struct Settings {
    file: StoragePath,
}

impl Settings {
    pub(crate) fn new(file: StoragePath) -> Self {
        Self { file }
    }

    /// 当前的完整设置（对象）。读不到或不是对象时为空对象。
    pub async fn read(&self) -> Map<String, Value> {
        match rust_cli_fs::read_optional(self.file.as_path()).await {
            Ok(Some(bytes)) => match serde_json::from_slice::<Value>(&bytes) {
                Ok(Value::Object(settings)) => settings,
                Ok(_) | Err(_) => {
                    tracing::warn!(file = %self.file.as_path().display(), "setting.json is not an object");
                    Map::new()
                }
            },
            Ok(None) => Map::new(),
            Err(error) => {
                tracing::warn!(%error, "cannot read setting.json");
                Map::new()
            }
        }
    }

    /// 设置页的 ZCode 端点覆盖 `zcodeEndpointOrigin`（去掉首尾空白，空值视为没有）。
    pub async fn zcode_endpoint_origin(&self) -> Option<String> {
        self.read()
            .await
            .get("zcodeEndpointOrigin")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|origin| !origin.is_empty())
            .map(str::to_owned)
    }

    /// App 的“完整保留模型 I/O”开关 `modelIoFullRetentionEnabled`（缺省关，只有 `true` 算开；工作线 F91，
    /// docs/specs/rust-cli/model-io.md §3.4）。模型轨迹的写者每写一条读一次。
    pub async fn model_io_full_retention(&self) -> bool {
        self.read().await.get("modelIoFullRetentionEnabled") == Some(&Value::Bool(true))
    }
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
