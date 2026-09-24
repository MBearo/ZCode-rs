//! Configuration file discovery and reading (Node `createConfig` IO side).
//! All interpretation is delegated to the pure `domain::config` module.
use crate::contract::ConfigSource;
use crate::domain::config::{
    ConfigSnapshot, Diagnostic, LoadedFile, Scope, effective, env_patch, merge_layers, parse_file,
    project_file, resolve_mcp,
};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{Map, Value};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

/// Upper bound for one config file; larger files are reported invalid instead of read.
const MAX_CONFIG_BYTES: u64 = 8 * 1024 * 1024;

/// Loads the layered configuration of one workspace on every call (Node reloads
/// config at each entry point and has no file watcher).
pub struct WorkspaceConfig {
    cwd: PathBuf,
    home: PathBuf,
    env: Vec<(String, String)>,
}

impl WorkspaceConfig {
    pub fn new(cwd: PathBuf, home: PathBuf, env: Vec<(String, String)>) -> Self {
        Self { cwd, home, env }
    }

    pub fn user_path(&self) -> PathBuf {
        self.home.join(".zcode/cli/config.json")
    }
}

async fn read(path: &Path) -> Result<Option<String>, String> {
    match tokio::fs::metadata(path).await {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
        Ok(meta) if meta.len() > MAX_CONFIG_BYTES => {
            return Err(format!("Config file exceeds {MAX_CONFIG_BYTES} bytes"));
        }
        Ok(_) => {}
    }
    tokio::fs::read_to_string(path)
        .await
        .map(Some)
        .map_err(|e| e.to_string())
}

async fn load_file(path: &Path) -> LoadedFile {
    let text = read(path).await;
    parse_file(
        &path.to_string_lossy(),
        text.as_ref().map(|t| t.as_deref()).map_err(Clone::clone),
    )
}

async fn has_worktree_marker(directory: &Path) -> bool {
    tokio::fs::metadata(directory.join(".git"))
        .await
        .is_ok_and(|m| m.is_dir() || m.is_file())
}

/// Node `discoverWorkspaceHookConfigPaths`: directories from the nearest worktree
/// root down to `cwd`, each contributing `zcode.json` then `.zcode/config.json`.
async fn project_paths(cwd: &Path) -> Vec<PathBuf> {
    let mut directories = vec![];
    let mut found = false;
    for directory in cwd.ancestors() {
        directories.push(directory.to_path_buf());
        if has_worktree_marker(directory).await {
            found = true;
            break;
        }
    }
    if !found {
        directories = vec![cwd.to_path_buf()];
    } else {
        directories.reverse();
    }
    let mut paths = vec![];
    for directory in directories {
        for candidate in [
            directory.join("zcode.json"),
            directory.join(".zcode/config.json"),
        ] {
            if tokio::fs::try_exists(&candidate).await.unwrap_or(false)
                && !paths.contains(&candidate)
            {
                paths.push(candidate);
            }
        }
    }
    paths
}

impl WorkspaceConfig {
    pub async fn snapshot(&self) -> ConfigSnapshot {
        let system = crate::domain::config::defaults();
        let user = load_file(&self.user_path()).await;
        let mut diagnostics: Vec<Diagnostic> = user.diagnostics.clone();
        let mut projects = vec![];
        let mut hook_candidates = vec![];
        for (discovery_order, path) in project_paths(&self.cwd).await.into_iter().enumerate() {
            let (file, hooks) = project_file(load_file(&path).await);
            diagnostics.extend(file.diagnostics.clone());
            if let Some(hooks) = hooks {
                hook_candidates.push(crate::domain::config::HookCandidate {
                    path: file.path.clone(),
                    discovery_order,
                    hooks,
                });
            }
            if file.loaded {
                projects.push(file);
            }
        }
        let env = env_patch(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        let mut layers: Vec<(Scope, &Map<String, Value>)> = vec![(Scope::System, &system)];
        if user.loaded {
            layers.push((Scope::User, &user.patch));
        }
        layers.extend(projects.iter().map(|file| (Scope::Project, &file.patch)));
        if !env.is_empty() {
            layers.push((Scope::Env, &env));
        }
        let mut merged = merge_layers(&layers);
        // MCP 服务器按 Node 规则单独解析：用户层覆盖项目层（项目层先按文件顺序合并）。
        let project_layers: Vec<_> = projects
            .iter()
            .map(|f| (Scope::Project, &f.patch))
            .collect();
        let project = merge_layers(&project_layers);
        let (servers, sources) = resolve_mcp(&[
            (Scope::System, &system),
            (Scope::Project, &project),
            (Scope::User, &user.patch),
            (Scope::Env, &env),
        ]);
        let mcp = merged
            .entry("mcp".to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        mcp["servers"] = Value::Object(servers);
        for diagnostic in &diagnostics {
            tracing::warn!(
                target: "zcode::config",
                event = "config.diagnostic",
                code = diagnostic.code,
                path = diagnostic.path.as_deref(),
                "Configuration diagnostic"
            );
        }
        ConfigSnapshot {
            config: effective(&merged),
            mcp_sources: sources,
            project_hook_candidates: hook_candidates,
            user_hooks: if user.loaded {
                user.patch.get("hooks").cloned().unwrap_or(Value::Null)
            } else {
                Value::Null
            },
            user_view: {
                let mut layers: Vec<(Scope, &Map<String, Value>)> = vec![(Scope::System, &system)];
                if user.loaded {
                    layers.push((Scope::User, &user.patch));
                }
                if !env.is_empty() {
                    layers.push((Scope::Env, &env));
                }
                effective(&merge_layers(&layers))
            },
            user_plugins: user.patch.get("plugins").cloned().unwrap_or(Value::Null),
            project_plugins: project.get("plugins").cloned().unwrap_or(Value::Null),
            user_path: user.path,
            project_paths: projects.into_iter().map(|f| f.path).collect(),
            diagnostics,
        }
    }
}

#[async_trait]
impl ConfigSource for WorkspaceConfig {
    async fn load(&self) -> Result<Arc<ConfigSnapshot>> {
        Ok(Arc::new(self.snapshot().await))
    }
    async fn set_workspace_hook_enabled(
        &self,
        toggle: crate::contract::HookToggle,
    ) -> std::result::Result<(), &'static str> {
        // 只允许改写当前工作目录的 .zcode/config.json（Node editable 规则）。
        if Path::new(&toggle.path) != self.cwd.join(".zcode").join("config.json") {
            return Err("workspace_hooks_snapshot_mismatch");
        }
        crate::hook_toggle::set_enabled(&toggle).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn layers_user_project_and_env_like_node() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let repo = root.path().join("repo");
        let nested = repo.join("pkg");
        tokio::fs::create_dir_all(home.join(".zcode/cli"))
            .await
            .unwrap();
        tokio::fs::create_dir_all(nested.join(".zcode"))
            .await
            .unwrap();
        tokio::fs::create_dir_all(repo.join(".git")).await.unwrap();
        let write = |path: PathBuf, value: Value| async move {
            tokio::fs::write(path, value.to_string()).await.unwrap()
        };
        write(
            home.join(".zcode/cli/config.json"),
            json!({"mcp":{"servers":{"shared":{"command":"user"}}},"network":{"httpProxy":"http://u"}}),
        )
        .await;
        write(
            repo.join("zcode.json"),
            json!({"mcp":{"servers":{"shared":{"command":"project"},"local":{"command":"x","cwd":"bin"}}}}),
        )
        .await;
        write(
            nested.join(".zcode/config.json"),
            json!({"hooks":{"enabled":true}}),
        )
        .await;
        let config = WorkspaceConfig::new(
            nested.clone(),
            home,
            vec![("ZCODE_NO_PROXY".into(), "*".into())],
        );
        let snapshot = config.snapshot().await;
        let servers = &snapshot.config["mcp"]["servers"];
        assert_eq!(servers["shared"]["command"], "user");
        assert_eq!(
            servers["local"]["cwd"],
            repo.join("bin").to_string_lossy().as_ref()
        );
        assert_eq!(snapshot.mcp_sources["local"], "project");
        // 与 Node 一致：env 层的 network 段整段替换用户层的 network。
        assert_eq!(
            snapshot.config["network"],
            json!({"noProxy":"*","timeout":180000})
        );
        assert_eq!(snapshot.project_hook_candidates.len(), 1);
        assert_eq!(snapshot.project_paths.len(), 2);
    }
}
