//! Node 已安装插件与官方内置插件缓存的只读清单（docs/specs/rust-cli/ext.md §4.1、§4.2）。
//!
//! 都在插件存储根（`StoragePaths::plugin_storage`）下，是本机存储，经 rust-cli-fs 读：
//! - `installed_plugins.json`：数组格式（`plugins` 是数组，Node 写的）与对象格式（`plugins` 是以 id 为键的对象）都认
//!   （Node `normalizeInstalledPluginsState`）；
//! - `marketplaces/zcode-plugins-official/bundled-marketplace.json`：Node 播种的内置插件分片，条目的
//!   `cachePath` 是插件目录；分片不存在时扫描 `cache/zcode-plugins-official/*/*`（Node `scanOfficialCache`）。
//!
//! 只读：不做 Node 的原子目录崩溃恢复（`recoverAtomicTargetSync`，ext.md D24）。插件目录里的内容由 ext
//! 经执行环境端口读。

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

// 按字面化简 `.` 与 `..`（Node `resolve` 的规范化；不访问文件系统）：唯一定义在 base（F58）。
use rust_cli_base::path::normalize;
use rust_cli_paths::StoragePath;
use serde_json::{Map, Value};

const INSTALLED_PLUGINS: &str = "installed_plugins.json";
use rust_cli_base::names::OFFICIAL_MARKETPLACE;
/// 存储根下的插件缓存目录（Node `cache/`；F55：以前写了三次）。
const CACHE_DIR: &str = "cache";
const BUNDLED_PARTITION: &str = "bundled-marketplace.json";
/// Node `DEFAULT_VERSION`。
const DEFAULT_VERSION: &str = "0.0.0";

/// 一条已安装插件的记录（Node `InstalledPluginRecord` 里加载用到的字段）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledPlugin {
    /// `<name>@<marketplace>`。
    pub id: String,
    pub name: String,
    pub marketplace: String,
    pub version: String,
    /// 记录里的插件目录；Node 格式里可以是空串（见 [`installed_root`]）。
    pub install_path: String,
}

/// 读 JSON 对象；文件不存在、读不出或不是对象时为 None（Node `readJsonFileSync` 返回 undefined）。
async fn read_object(path: &Path) -> Option<Map<String, Value>> {
    match rust_cli_fs::read_optional(path).await {
        Ok(Some(bytes)) => match serde_json::from_slice(&bytes) {
            Ok(Value::Object(object)) => Some(object),
            _ => None,
        },
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "plugin inventory file could not be read");
            None
        }
    }
}

fn text<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    object.get(key).and_then(Value::as_str)
}

/// Node 格式的一条记录（`isInstalledPluginRecord`：七个字段的类型都对）。
fn node_record(value: &Value) -> Option<InstalledPlugin> {
    let object = value.as_object()?;
    let scope = text(object, "scope")?;
    text(object, "installedAt")?;
    if scope != "user" && scope != "workspace" {
        return None;
    }
    Some(InstalledPlugin {
        id: text(object, "id")?.to_owned(),
        name: text(object, "name")?.to_owned(),
        marketplace: text(object, "marketplace")?.to_owned(),
        version: text(object, "version")?.to_owned(),
        install_path: text(object, "installPath")?.to_owned(),
    })
}

/// 对象格式的一个键与它的条目（`normalizeInstalledPluginRecordFromMap`）：id 按最后一个 `@` 切分，
/// `installPath` 不能为空。
fn map_records(id: &str, entry: &Value) -> Vec<InstalledPlugin> {
    let Some((name, marketplace)) = id.rsplit_once('@') else {
        return Vec::new();
    };
    if name.is_empty() || marketplace.is_empty() {
        return Vec::new();
    }
    let entries = match entry {
        Value::Array(items) => items.iter().collect(),
        other => vec![other],
    };
    entries
        .into_iter()
        .filter_map(Value::as_object)
        .filter_map(|item| {
            let install_path = text(item, "installPath").filter(|path| !path.is_empty())?;
            Some(InstalledPlugin {
                id: id.to_owned(),
                name: name.to_owned(),
                marketplace: marketplace.to_owned(),
                version: text(item, "version").unwrap_or(DEFAULT_VERSION).to_owned(),
                install_path: install_path.to_owned(),
            })
        })
        .collect()
}

/// `installed_plugins.json` 的记录，按文件里的顺序；文件不存在或不合法时为空。
pub async fn installed_plugins(root: &StoragePath) -> Vec<InstalledPlugin> {
    let Some(state) = read_object(&root.as_path().join(INSTALLED_PLUGINS)).await else {
        return Vec::new();
    };
    match state.get("plugins") {
        Some(Value::Object(map)) => map
            .iter()
            .flat_map(|(id, entry)| map_records(id, entry))
            .collect(),
        Some(Value::Array(items)) => items.iter().filter_map(node_record).collect(),
        _ => Vec::new(),
    }
}

/// Node `sanitizePluginId`：`[^a-zA-Z0-9_.@-]` 换成 `-`。
fn sanitize(segment: &str) -> String {
    segment
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '@' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// 已安装插件的目录（Node `resolveInstalledPluginRoot`）：`installPath`，为空时
/// `<存储根>/cache/<marketplace>/<name>/<version>`（各段净化）。
pub fn installed_root(root: &StoragePath, plugin: &InstalledPlugin) -> PathBuf {
    if !plugin.install_path.is_empty() {
        return PathBuf::from(&plugin.install_path);
    }
    root.as_path()
        .join(CACHE_DIR)
        .join(sanitize(&plugin.marketplace))
        .join(sanitize(&plugin.name))
        .join(sanitize(&plugin.version))
}

/// bundled 分片里的插件目录（Node `loadBundledOfficialPluginRootsSync`）：`cachePath` 必须在
/// `<存储根>/cache/zcode-plugins-official/<name>/` 之下（严格的后代），名字不能越出缓存目录。分片不存在、
/// 版本不是 1 或 `manifest` 不是对象时为 None。
fn bundled_roots(root: &StoragePath, partition: &Map<String, Value>) -> Option<Vec<PathBuf>> {
    if partition.get("version").and_then(Value::as_u64) != Some(1) {
        return None;
    }
    let manifest = partition.get("manifest")?.as_object()?;
    let cache = root.as_path().join(CACHE_DIR).join(OFFICIAL_MARKETPLACE);
    let plugins = manifest.get("plugins").and_then(Value::as_array);
    let roots = plugins
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .filter_map(|plugin| {
            let name = text(plugin, "name").filter(|name| !name.is_empty())?;
            let cache_path = text(plugin, "cachePath")?;
            let plugin_root = normalize(&cache.join(name));
            let resolved = normalize(Path::new(cache_path));
            let inside = |parent: &Path, child: &Path| {
                child
                    .strip_prefix(parent)
                    .is_ok_and(|rest| rest.components().next().is_some())
            };
            (inside(&cache, &plugin_root) && inside(&plugin_root, &resolved)).then_some(resolved)
        })
        .collect();
    Some(roots)
}

/// 官方内置插件的目录：bundled 分片的 `cachePath`，按分片顺序；分片不存在时扫描
/// `cache/zcode-plugins-official/<插件>/<版本>`，按名字排序（Node 按目录顺序）。
pub async fn official_plugin_roots(root: &StoragePath) -> Vec<PathBuf> {
    let partition = root
        .as_path()
        .join("marketplaces")
        .join(OFFICIAL_MARKETPLACE)
        .join(BUNDLED_PARTITION);
    if let Some(partition) = read_object(&partition).await
        && let Some(roots) = bundled_roots(root, &partition)
    {
        return roots;
    }
    let cache = root.as_path().join(CACHE_DIR).join(OFFICIAL_MARKETPLACE);
    let mut roots = Vec::new();
    for plugin in subdirectories(&cache).await {
        let dir = cache.join(plugin);
        roots.extend(subdirectories(&dir).await.into_iter().map(|v| dir.join(v)));
    }
    roots
}

/// 子目录名；目录不存在时为空，其余错误记警告后为空（Node `plugin_root_not_found` 诊断）。
async fn subdirectories(dir: &Path) -> Vec<String> {
    match rust_cli_fs::subdirectories(dir).await {
        Ok(names) => names,
        Err(error) if error.kind() == ErrorKind::NotFound => Vec::new(),
        Err(error) => {
            tracing::warn!(dir = %dir.display(), %error, "the official plugin cache could not be scanned");
            Vec::new()
        }
    }
}

#[cfg(test)]
#[path = "plugin_inventory_tests.rs"]
mod tests;
