//! 会话库与存储根的位置（修复线 F2，docs/specs/rust-cli/config.md §2.3）：Node `createConfig` 的
//! `storage.sessionDbPath` 与 `storage.dir`，按用户级 → 项目级 → 环境变量的顺序，后者覆盖前者（Node
//! `ConfigStore.merge` 跳过空值）。环境变量取入口捕获的进程环境快照：`ZCODE_STORAGE_DIR`、
//! `ZCODE_SESSION_DB_PATH` 与 `ZCODE_SESSION_DB`（两个都有时按环境里的先后，后出现的覆盖，Node `parseEnvConfig`
//! 按 `Object.entries(process.env)` 的顺序）。以前入口不读这些值，库与产物目录总在缺省位置。

use rust_cli_kernel_api::permission::RuleSource;
use rust_cli_paths::StorageSettings;
use serde_json::Value;

use super::merge::Layer;

/// 合并后的两个值（原文，`~` 与相对路径由 `StoragePaths::resolve` 按 Node `resolvePath` 展开）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct StorageLayers {
    pub(crate) dir: Option<String>,
    pub(crate) session_db_path: Option<String>,
}

impl StorageLayers {
    pub(crate) fn from_layers(
        layers: &[Layer<'_>],
        process_env: &rust_cli_base::secret::EnvSnapshot,
    ) -> Self {
        Self {
            dir: merged(
                layers,
                process_env,
                "dir",
                &[rust_cli_base::names::env::ZCODE_STORAGE_DIR],
            ),
            session_db_path: merged(
                layers,
                process_env,
                "sessionDbPath",
                &[
                    rust_cli_base::names::env::ZCODE_SESSION_DB_PATH,
                    rust_cli_base::names::env::ZCODE_SESSION_DB,
                ],
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

/// 主 env 不在本机时能决定 controller 存储位置的层：只有用户级配置。其余的层（执行侧的项目配置、执行侧读来的
/// `--settings`）里的存储键忽略并告警（deployment.md §21）。
pub(crate) fn controller_layers<'a>(
    layers: &[Layer<'a>],
    warnings: &mut Vec<String>,
) -> Vec<Layer<'a>> {
    let mut own = Vec::new();
    for layer in layers {
        if layer.source == RuleSource::UserSettings {
            own.push(*layer);
            continue;
        }
        let Some(storage) = layer.object.get("storage").and_then(Value::as_object) else {
            continue;
        };
        for key in ["dir", "sessionDbPath"] {
            if storage.contains_key(key) {
                warnings.push(format!(
                    "{}: storage.{key} is ignored: the file is on the primary env, which cannot move this machine's storage",
                    layer.path
                ));
            }
        }
    }
    own
}

/// 最后一个非空的值：环境变量（几个名字里，环境里后出现的那个）优先，否则是最后写这个键的配置层。按快照的顺序找，
/// 不是按名字取一个值，所以经 `vars`。
fn merged(
    layers: &[Layer<'_>],
    process_env: &rust_cli_base::secret::EnvSnapshot,
    key: &str,
    names: &[&str],
) -> Option<String> {
    let from_env = process_env
        .vars()
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
