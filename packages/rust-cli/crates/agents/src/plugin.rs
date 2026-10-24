//! 插件提供的 agent（docs/specs/rust-cli/agents.md §3）。
//!
//! 文件位置由 ext 在插件加载时给出（runtime 经 [`crate::AgentCatalog::set_plugins`] 交进来）；内容与
//! 用户 / 项目 agent 一样每次经执行环境读。与自定义 agent 的不同：类型名带插件名与命名空间、描述有
//! 缺省句、不认 `permissionMode`、正文替换插件与项目目录变量。

use std::collections::BTreeMap;

use rust_cli_base::names::vars;
use rust_cli_kernel_api::env::{EnvPath, ReadFile, WorkspaceRef};
use rust_cli_kernel_api::machine::Files;

use crate::definitions::{AgentDefinition, AgentSource};
use crate::frontmatter::{self, Value};

/// 插件里的一个 agent 文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginAgentFile {
    /// 插件名（类型名的第一段）。
    pub plugin: String,
    /// 插件目录（`${CLAUDE_PLUGIN_ROOT}`）。
    pub root: String,
    pub path: String,
    /// 相对 agents 目录的子目录（namespace）。
    pub namespace: Vec<String>,
}

fn file_stem(path: &str) -> &str {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    name.strip_suffix(".md").unwrap_or(name)
}

/// 插件目录与项目目录变量（两套名字都认，与 ext 的 MCP 配置相同）。
fn substitute(text: &str, root: &str, project_dir: &str) -> String {
    [
        (vars::CLAUDE_PLUGIN_ROOT, root),
        (vars::ZCODE_PLUGIN_ROOT, root),
        (vars::CLAUDE_PROJECT_DIR, project_dir),
        (vars::ZCODE_PROJECT_DIR, project_dir),
    ]
    .iter()
    .fold(text.to_owned(), |text, (name, value)| {
        text.replace(&vars::placeholder(name), value)
    })
}

/// 按插件规则解析一个 agent 文件；总有结果（没有 frontmatter 时名字取文件名、描述用缺省句）。
pub(crate) fn parse(raw: &str, file: &PluginAgentFile, project_dir: &str) -> AgentDefinition {
    let (map, body) = frontmatter::split(raw).unwrap_or_else(|| (BTreeMap::new(), raw.to_owned()));
    let text = |key: &str| {
        map.get(key)
            .and_then(Value::text)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    let name = text("name").unwrap_or_else(|| file_stem(&file.path));
    let agent_type = std::iter::once(file.plugin.as_str())
        .chain(file.namespace.iter().map(String::as_str))
        .chain(std::iter::once(name))
        .collect::<Vec<_>>()
        .join(":");
    let when_to_use = ["description", "when_to_use", "when-to-use"]
        .into_iter()
        .find_map(text)
        .map_or_else(
            || format!("Agent from {} plugin", file.plugin),
            |description| description.replace("\\n", "\n"),
        );
    let model = text("model").map(|model| {
        if model.eq_ignore_ascii_case("inherit") {
            "inherit".to_owned()
        } else {
            model.to_owned()
        }
    });
    if ["permissionMode", "hooks", "mcpServers"]
        .iter()
        .any(|key| map.contains_key(*key))
    {
        tracing::warn!(
            path = file.path,
            "plugin agent sets permissionMode, hooks or mcpServers, which are ignored"
        );
    }
    AgentDefinition {
        agent_type,
        when_to_use,
        tools: frontmatter::tools(map.get("tools")),
        disallowed_tools: frontmatter::tools(map.get("disallowedTools")).unwrap_or_default(),
        prompt: substitute(body.trim(), &file.root, project_dir),
        source: AgentSource::Plugin,
        model,
        effort: text("effort").map(str::to_owned),
        max_turns: text("maxTurns")
            .and_then(|turns| turns.parse::<u32>().ok())
            .filter(|turns| *turns > 0),
        // 插件 agent 不认 permissionMode（插件是第三方代码）。
        permission_mode: None,
        background: text("background") == Some("true"),
    }
}

/// 读出并解析插件 agent 文件；读不了的文件跳过。
pub(crate) async fn load(
    port: &dyn Files,
    workspace: &WorkspaceRef,
    files: &[PluginAgentFile],
) -> Vec<AgentDefinition> {
    let mut found = Vec::new();
    for file in files {
        let request = ReadFile {
            path: EnvPath::new(file.path.as_str()),
        };
        match port.read_file(workspace, request).await {
            Ok(content) => {
                let raw = String::from_utf8_lossy(&content.bytes);
                found.push(parse(&raw, file, workspace.root.as_str()));
            }
            Err(error) => tracing::debug!(%error, path = file.path, "plugin agent not readable"),
        }
    }
    found
}

#[cfg(test)]
#[path = "plugin_tests.rs"]
mod tests;
