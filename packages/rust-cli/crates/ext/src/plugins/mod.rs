//! 插件（docs/specs/rust-cli/ext.md §4）：目录与 manifest，提供 skill、MCP 服务器、agent 文件与 hooks。
//! 候选目录由入口层给出（`--plugin-dir`、Node 配置的 `plugins.dirs`、内置缓存与已安装插件），文件经执行
//! 环境端口读；来源与启用判定见 [`select`]。市场与安装未覆盖。
//!
//! manifest 按 Node `findManifest` 依次找本工程的 `.zcode-plugin` 与另外两种插件格式的目录下的 `plugin.json`
//! （同时兼容几种插件格式）；`--plugin-dir` 没有 manifest 时插件名取目录名。

mod select;

use rust_cli_base::names::vars;
use rust_cli_kernel_api::env::{EnvPath, ReadFile, WorkspaceRef};
use rust_cli_kernel_api::env_files::{FileKind, ListDir, PathStyle, StatFile};
use rust_cli_kernel_api::machine::Files;
use rust_cli_kernel_api::mcp::McpTransportConfig;
use serde_json::Value;

use crate::mcp::{McpScope, McpServer, parse_servers};
use crate::skills::PluginSkillRoot;

pub use self::select::{
    Loaded, NodeDependency, PluginCandidate, PluginOrigin, PluginSources, SkipReason, Skipped,
    load_all,
};

/// manifest 的位置，按 Node `findManifest` 的顺序。
const MANIFESTS: [&str; 3] = [
    ".zcode-plugin/plugin.json",
    ".claude-plugin/plugin.json",
    ".codex-plugin/plugin.json",
];

/// 读 manifest 的结果。
enum Manifest {
    Missing,
    Invalid(String),
    Found(Value),
}

/// 插件里的一个 agent 文件（agents.md §3 按插件规则解析）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginAgentFile {
    pub path: String,
    /// 相对 agents 目录的子目录（namespace）；manifest 直接列出的文件为空。
    pub namespace: Vec<String>,
}

/// 一个已加载的插件。
#[derive(Debug, Clone, PartialEq)]
pub struct Plugin {
    pub name: String,
    pub dir: String,
    pub manifest: Value,
    pub skill_dirs: Vec<String>,
    pub mcp_servers: Vec<McpServer>,
    /// agent 文件（runtime 交给 agents 的 `AgentCatalog::set_plugins`）。
    pub agent_files: Vec<PluginAgentFile>,
    /// `hooks/hooks.json` 与 manifest `hooks` 的内容，都是文件的 `{"hooks": …}` 形状（runtime 交给
    /// hooks 引擎的 `register_plugins`）。
    pub hooks: Vec<Value>,
}

/// 全部插件。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plugins(pub Vec<Plugin>);

impl Plugins {
    pub fn skill_roots(&self) -> Vec<PluginSkillRoot> {
        self.0
            .iter()
            .flat_map(|plugin| {
                plugin.skill_dirs.iter().map(|dir| PluginSkillRoot {
                    plugin: plugin.name.clone(),
                    dir: dir.clone(),
                })
            })
            .collect()
    }

    pub fn mcp_servers(&self) -> Vec<McpServer> {
        self.0
            .iter()
            .flat_map(|plugin| plugin.mcp_servers.iter().cloned())
            .collect()
    }
}

/// 插件目录下的路径（`path.join`）：风格按插件目录的写法推断，规则是 kernel-api `env_path`
/// （R1-9：以前按“含不含 `\`”猜风格，自己拼接）。
fn join(base: &str, relative: &str) -> String {
    PathStyle::infer(base).join(base, &[relative])
}

fn base_name(path: &str) -> String {
    PathStyle::infer(path).file_name(path)
}

/// manifest 里的路径字段：字符串或字符串数组。
fn paths(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::String(path)) => vec![path.clone()],
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    }
}

struct Reader<'a> {
    files: &'a dyn Files,
    workspace: &'a WorkspaceRef,
}

impl Reader<'_> {
    async fn kind(&self, path: &str) -> Option<FileKind> {
        let request = StatFile {
            path: EnvPath::new(path),
        };
        self.files
            .stat(self.workspace, request)
            .await
            .ok()
            .flatten()
            .map(|stat| stat.kind)
    }

    async fn parse(&self, path: &str) -> Result<Value, String> {
        let request = ReadFile {
            path: EnvPath::new(path),
        };
        let content = self
            .files
            .read_file(self.workspace, request)
            .await
            .map_err(|error| error.to_string())?;
        serde_json::from_slice(&content.bytes).map_err(|error| error.to_string())
    }

    async fn json(&self, path: &str) -> Option<Value> {
        match self.parse(path).await {
            Ok(value) => Some(value),
            Err(error) => {
                tracing::warn!(path, %error, "plugin file is not valid JSON");
                None
            }
        }
    }

    /// 目录里的 `.md` 文件，递归进子目录，子目录名记作命名空间；按名字
    /// 排序，结果确定。
    async fn markdown(&self, root: &str) -> Vec<PluginAgentFile> {
        let mut found = Vec::new();
        let mut pending = vec![(root.to_owned(), Vec::<String>::new())];
        while let Some((dir, namespace)) = pending.pop() {
            let request = ListDir {
                path: EnvPath::new(dir.as_str()),
            };
            let Ok(mut entries) = self.files.list_dir(self.workspace, request).await else {
                continue;
            };
            entries.sort_by(|left, right| left.name.cmp(&right.name));
            let mut subdirs = Vec::new();
            for entry in entries {
                let path = join(&dir, &entry.name);
                match entry.kind {
                    FileKind::Directory => {
                        let mut inner = namespace.clone();
                        inner.push(entry.name);
                        subdirs.push((path, inner));
                    }
                    FileKind::File if entry.name.to_lowercase().ends_with(".md") => {
                        found.push(PluginAgentFile {
                            path,
                            namespace: namespace.clone(),
                        });
                    }
                    _ => {}
                }
            }
            // 先处理本层文件，再按名字顺序进子目录。
            pending.extend(subdirs.into_iter().rev());
        }
        found
    }

    async fn manifest(&self, dir: &str) -> Manifest {
        for candidate in MANIFESTS {
            let path = join(dir, candidate);
            if self.kind(&path).await == Some(FileKind::File) {
                return match self.parse(&path).await {
                    Ok(value) => Manifest::Found(value),
                    Err(error) => Manifest::Invalid(format!("{path}: {error}")),
                };
            }
        }
        Manifest::Missing
    }

    /// MCP 服务器声明：`.mcp.json` 与 manifest 的 `mcpServers`（内联对象，或指向 JSON 文件的路径）；每张
    /// 表已去掉外层的 `mcpServers`。
    async fn mcp_tables(&self, dir: &str, manifest: &Value) -> Vec<Value> {
        let mut tables = Vec::new();
        let default = join(dir, ".mcp.json");
        if self.kind(&default).await == Some(FileKind::File)
            && let Some(file) = self.json(&default).await
        {
            tables.push(file);
        }
        match manifest.get("mcpServers") {
            Some(Value::Object(inline)) => tables.push(Value::Object(inline.clone())),
            other => {
                for path in paths(other) {
                    if let Some(file) = self.json(&join(dir, &path)).await {
                        tables.push(file);
                    }
                }
            }
        }
        tables
            .into_iter()
            .map(|table| table.get("mcpServers").cloned().unwrap_or(table))
            .collect()
    }

    /// `project` 是工作区根：stdio 服务器的环境按 Node（`adapters/src/plugins/mcp.ts`）给插件目录与项目目录，两套
    /// 名字，配置里的 `env` 优先（修复线 F42，config.md §2.5.1；以前不给）。数据目录与插件 ID 不给（ext.md D13）。
    fn mcp_servers(plugin: &str, dir: &str, project: &str, tables: &[Value]) -> Vec<McpServer> {
        let root = dir.to_owned();
        let lookup = move |name: &str| {
            matches!(name, vars::CLAUDE_PLUGIN_ROOT | vars::ZCODE_PLUGIN_ROOT).then(|| root.clone())
        };
        let provided = [
            (vars::CLAUDE_PLUGIN_ROOT, dir),
            (vars::ZCODE_PLUGIN_ROOT, dir),
            (vars::CLAUDE_PROJECT_DIR, project),
            (vars::ZCODE_PROJECT_DIR, project),
        ];
        let mut servers = Vec::new();
        for table in tables {
            let (found, warnings) = parse_servers(table, McpScope::Plugin, &lookup);
            for warning in warnings {
                tracing::warn!(plugin, warning, "plugin MCP configuration");
            }
            servers.extend(found.into_iter().map(|mut server| {
                if let McpTransportConfig::Stdio { env, .. } = &mut server.transport {
                    for (name, value) in provided {
                        env.entry(name.to_owned())
                            .or_insert_with(|| value.to_owned());
                    }
                }
                McpServer {
                    name: format!("plugin:{plugin}:{}", server.name),
                    ..server
                }
            }));
        }
        servers
    }

    /// `--plugin-dir` 的插件：manifest 缺失、读不出或没有 `name` 时插件名取目录名。
    async fn session_plugin(&self, dir: &str) -> Option<Plugin> {
        if self.kind(dir).await != Some(FileKind::Directory) {
            tracing::warn!(dir, "plugin directory not found");
            return None;
        }
        let manifest = match self.manifest(dir).await {
            Manifest::Found(value) => value,
            Manifest::Missing => Value::Null,
            Manifest::Invalid(error) => {
                tracing::warn!(dir, error, "plugin manifest is not valid JSON");
                Value::Null
            }
        };
        let name = manifest
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .map_or_else(|| base_name(dir), str::to_owned);
        let tables = self.mcp_tables(dir, &manifest).await;
        Some(self.components(dir, name, manifest, &tables).await)
    }

    /// 插件的组件（ext.md §4.5）。`tables` 是 [`Self::mcp_tables`] 的结果。
    async fn components(
        &self,
        dir: &str,
        name: String,
        manifest: Value,
        tables: &[Value],
    ) -> Plugin {
        let mut skill_dirs = Vec::new();
        for candidate in std::iter::once("skills".to_owned()).chain(paths(manifest.get("skills"))) {
            let path = join(dir, &candidate);
            if self.kind(&path).await == Some(FileKind::Directory) && !skill_dirs.contains(&path) {
                skill_dirs.push(path);
            }
        }
        let mut agent_files: Vec<PluginAgentFile> = Vec::new();
        for candidate in std::iter::once("agents".to_owned()).chain(paths(manifest.get("agents"))) {
            let path = join(dir, &candidate);
            let found = match self.kind(&path).await {
                Some(FileKind::Directory) => self.markdown(&path).await,
                Some(FileKind::File) if path.to_lowercase().ends_with(".md") => {
                    vec![PluginAgentFile {
                        path,
                        namespace: Vec::new(),
                    }]
                }
                _ => Vec::new(),
            };
            // 同一文件只取一次。
            for file in found {
                if !agent_files.iter().any(|known| known.path == file.path) {
                    agent_files.push(file);
                }
            }
        }
        let mut hooks = Vec::new();
        let default_hooks = join(dir, "hooks/hooks.json");
        if self.kind(&default_hooks).await == Some(FileKind::File)
            && let Some(file) = self.json(&default_hooks).await
        {
            hooks.push(file);
        }
        match manifest.get("hooks") {
            // 内联对象是事件表，包成文件的形状（`hooks` 字段）。
            Some(Value::Object(inline)) => {
                hooks.push(serde_json::json!({"hooks": Value::Object(inline.clone())}));
            }
            other => {
                for path in paths(other) {
                    hooks.extend(self.json(&join(dir, &path)).await);
                }
            }
        }
        let mcp_servers = Self::mcp_servers(&name, dir, self.workspace.root.as_str(), tables);
        Plugin {
            name,
            dir: dir.to_owned(),
            manifest,
            skill_dirs,
            mcp_servers,
            agent_files,
            hooks,
        }
    }
}

#[cfg(test)]
#[path = "plugins_tests.rs"]
mod tests;
