use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::path::{Component, Path, PathBuf};
use tokio::io::AsyncReadExt;

pub(super) fn home() -> PathBuf {
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default()
}
pub(super) fn resolve(base: &Path, path: &str) -> PathBuf {
    let raw = path
        .strip_prefix("~/")
        .map(|p| home().join(p))
        .unwrap_or_else(|| base.join(path));
    let mut result = PathBuf::new();
    for part in raw.components() {
        match part {
            Component::CurDir => (),
            Component::ParentDir => {
                result.pop();
            }
            other => result.push(other),
        }
    }
    result
}
pub(super) async fn json_file(path: &Path) -> Result<Value> {
    let file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(json!({})),
        Err(e) => return Err(e.into()),
    };
    let mut bytes = vec![];
    file.take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .await?;
    ensure!(
        bytes.len() <= 4 * 1024 * 1024,
        "Extension configuration exceeds size limit"
    );
    Ok(serde_json::from_slice(&bytes)?)
}
pub(super) async fn project_directories(cwd: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![];
    for dir in cwd.ancestors() {
        dirs.push(dir.to_owned());
        if tokio::fs::metadata(dir.join(".git")).await.is_ok() {
            return dirs;
        }
    }
    vec![cwd.to_owned()]
}
pub(super) async fn load(cwd: &Path) -> Result<Value> {
    let mut config = json_file(&home().join(".zcode/cli/config.json")).await?;
    for dir in project_directories(cwd).await.into_iter().rev() {
        for file in ["zcode.json", ".zcode/config.json"] {
            let mut next = json_file(&dir.join(file)).await?;
            if let Some(servers) = next["mcp"]["servers"].as_object_mut() {
                for server in servers.values_mut() {
                    if server["command"].is_string() {
                        server["cwd"] = resolve(&dir, server["cwd"].as_str().unwrap_or("."))
                            .to_string_lossy()
                            .into_owned()
                            .into();
                    }
                }
            }
            merge(&mut config, &next, 0);
        }
    }
    Ok(config)
}
fn merge(base: &mut Value, next: &Value, depth: usize) {
    let Some(object) = next.as_object() else {
        *base = next.clone();
        return;
    };
    if !base.is_object() {
        *base = json!({});
    }
    for (key, value) in object {
        // 顶层类别及服务器/插件字典按 key 合并；单个 server/override 是完整配置。
        if depth < 2 && value.is_object() {
            merge(&mut base[key], value, depth + 1);
        } else {
            base[key] = value.clone();
        }
    }
}
pub(super) fn storage(config: &Value) -> PathBuf {
    let base = std::env::var("ZCODE_STORAGE_DIR")
        .ok()
        .or_else(|| config["storage"]["dir"].as_str().map(str::to_owned));
    let base = base
        .map(|p| resolve(&home(), &p))
        .unwrap_or_else(|| home().join(".zcode"));
    if base.file_name().is_some_and(|p| p == "cli") {
        base.join("plugins")
    } else {
        base.join("cli/plugins")
    }
}
pub(super) fn strings(value: &Value) -> Vec<&str> {
    if let Some(s) = value.as_str() {
        vec![s]
    } else {
        value
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default()
    }
}
