//! 会话库与存储根的位置（修复线 F2，docs/specs/rust-cli/config.md §2.3）：Node `createConfig` 的
//! `storage.sessionDbPath` 与 `storage.dir`，按用户级 → 项目级 → 环境变量的顺序，后者覆盖前者（Node
//! `ConfigStore.merge` 跳过空值）。环境变量取入口捕获的进程环境快照：`ZCODE_STORAGE_DIR`、
//! `ZCODE_SESSION_DB_PATH` 与 `ZCODE_SESSION_DB`（两个都有时按环境里的先后，后出现的覆盖，Node `parseEnvConfig`
//! 按 `Object.entries(process.env)` 的顺序）。以前入口不读这些值，库与产物目录总在缺省位置。

use rust_cli_paths::StorageSettings;

use super::merge::Layer;

/// 合并后的两个值（原文，`~` 与相对路径由 `StoragePaths::resolve` 按 Node `resolvePath` 展开）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct StorageLayers {
    pub(crate) dir: Option<String>,
    pub(crate) session_db_path: Option<String>,
}

impl StorageLayers {
    pub(crate) fn from_layers(layers: &[Layer<'_>], process_env: &[(String, String)]) -> Self {
        Self {
            dir: merged(layers, process_env, "dir", &["ZCODE_STORAGE_DIR"]),
            session_db_path: merged(
                layers,
                process_env,
                "sessionDbPath",
                &["ZCODE_SESSION_DB_PATH", "ZCODE_SESSION_DB"],
            ),
        }
    }

    pub(crate) fn settings(&self) -> StorageSettings<'_> {
        StorageSettings {
            session_db_path: self.session_db_path.as_deref(),
            storage_dir: self.dir.as_deref(),
        }
    }
}

/// 最后一个非空的值：环境变量（环境里后出现的）优先，否则是最后写这个键的配置层。
fn merged(
    layers: &[Layer<'_>],
    process_env: &[(String, String)],
    key: &str,
    names: &[&str],
) -> Option<String> {
    let from_env = process_env
        .iter()
        .rev()
        .find(|(name, value)| names.contains(&name.as_str()) && !value.is_empty())
        .map(|(_, value)| value.clone());
    from_env.or_else(|| {
        layers.iter().rev().find_map(|layer| {
            let value = layer.object.get("storage")?.get(key)?.as_str()?;
            (!value.is_empty()).then(|| value.to_owned())
        })
    })
}

#[cfg(test)]
#[path = "storage_tests.rs"]
mod tests;
