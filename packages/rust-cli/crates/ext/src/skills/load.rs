//! skill 目录的发现与 SKILL.md 的解析（docs/specs/rust-cli/ext.md §2.1、§2.2）。

use std::sync::Arc;

use rust_cli_base::names::{DOT_DIR, managed_dir};
use rust_cli_kernel_api::env::{EnvFacts, EnvPath, ReadFile, WorkspaceRef};
use rust_cli_kernel_api::env_files::{FileKind, ListDir, PathStyle, StatFile};
use rust_cli_kernel_api::env_git::project_levels;
use rust_cli_kernel_api::machine::Files;

use super::frontmatter::{self, Value};
use super::{PluginSkillRoot, Skill, SkillCatalog, SkillShell, SkillSource};

/// SKILL.md 的大小上限（1e6 字节），超过时跳过。
const MAX_SKILL_BYTES: u64 = 1_000_000;

/// 每个根目录下的 skill 目录（`<根>/.zcode/skills`、`<根>/.agents/skills`）。
const SKILLS_DIR: &str = "skills";

/// 一个根目录与它的来源。
struct Root {
    dir: String,
    source: SkillSource,
}

fn base_roots(style: PathStyle, base: &str, source: &SkillSource) -> [Root; 2] {
    // `.zcode` 与 `.agents` 合并，同级 `.zcode` 在前（Node `skillRootsForBase`）。
    [DOT_DIR, ".agents"].map(|dot| Root {
        dir: style.join(base, &[dot, SKILLS_DIR]),
        source: source.clone(),
    })
}

struct Reader<'a> {
    catalog: &'a SkillCatalog,
    files: &'a dyn Files,
    workspace: &'a WorkspaceRef,
    /// 路径规则按执行环境的 platform（kernel-api `env_path`）。修复（R1-9）：以前按“路径含不含 `\`”
    /// 猜风格，`C:\a` 的上一层算成 `C:`。
    style: PathStyle,
}

impl Reader<'_> {
    /// cwd 向上到 git 根（含）或 home（不含）的每一层，离 cwd 近的在前。遍历在 kernel-api（F58：以前这里与
    /// agents 各写一份）；`.git` 只认目录与文件
    /// （缺陷修复：以前任何种类都算，FIFO 等会让遍历提前停下）。
    async fn project_levels(&self, facts: &EnvFacts) -> Vec<String> {
        let home = facts.home.as_ref().map(EnvPath::as_str);
        project_levels(
            self.files,
            self.workspace,
            self.style,
            facts.cwd.as_str(),
            home,
        )
        .await
    }

    /// 一个根目录里的 skill，按名字排序（`localeCompare`）。
    async fn load(&self, root: &Root, prefix: Option<&str>) -> Vec<Arc<Skill>> {
        let request = ListDir {
            path: EnvPath::new(&root.dir),
        };
        let Ok(mut entries) = self.files.list_dir(self.workspace, request).await else {
            return Vec::new();
        };
        entries.retain(|entry| entry.kind == FileKind::Directory);
        entries.sort_by_key(|entry| (entry.name.to_lowercase(), entry.name.clone()));
        let mut found = Vec::new();
        for entry in entries {
            let dir = self.style.join(&root.dir, &[&entry.name]);
            let name = match prefix {
                Some(plugin) => format!("{plugin}:{}", entry.name),
                None => entry.name.clone(),
            };
            if let Some(skill) = self.skill(&dir, name, &root.source).await {
                found.push(skill);
            }
        }
        found
    }

    async fn skill(&self, dir: &str, name: String, source: &SkillSource) -> Option<Arc<Skill>> {
        let file = self.style.join(dir, &["SKILL.md"]);
        let request = StatFile {
            path: EnvPath::new(&file),
        };
        let stat = self
            .files
            .stat(self.workspace, request)
            .await
            .ok()
            .flatten()?;
        if stat.kind != FileKind::File {
            return None;
        }
        if stat.size > MAX_SKILL_BYTES {
            tracing::warn!(
                file,
                size = stat.size,
                "skipping a skill over the size limit"
            );
            return None;
        }
        if let Some(skill) = self.catalog.cached(&file, &stat.version) {
            return Some(skill);
        }
        let request = ReadFile {
            path: EnvPath::new(&file),
        };
        let content = self.files.read_file(self.workspace, request).await.ok()?;
        let raw = String::from_utf8_lossy(&content.bytes);
        let skill = Arc::new(parse(&raw, name, dir, source.clone()));
        self.catalog.remember(file, content.version, skill.clone());
        Some(skill)
    }
}

/// 全部来源的 skill，按 ext.md §2.1 的顺序拼接（未去重）。
pub(crate) async fn discover(
    catalog: &SkillCatalog,
    workspace: &WorkspaceRef,
    facts: &EnvFacts,
    plugins: &[PluginSkillRoot],
) -> Vec<Arc<Skill>> {
    let style = PathStyle::of_platform(&facts.platform);
    let reader = Reader {
        catalog,
        files: catalog.files.as_ref(),
        workspace,
        style,
    };
    let mut roots = vec![Root {
        dir: style.join(managed_dir(&facts.platform), &[DOT_DIR, SKILLS_DIR]),
        source: SkillSource::Managed,
    }];
    if let Some(home) = &facts.home {
        roots.extend(base_roots(style, home.as_str(), &SkillSource::User));
    }
    for level in reader.project_levels(facts).await {
        roots.extend(base_roots(style, &level, &SkillSource::Project));
    }
    let mut found = Vec::new();
    for root in &roots {
        found.extend(reader.load(root, None).await);
    }
    for plugin in plugins {
        let root = Root {
            dir: plugin.dir.clone(),
            source: SkillSource::Plugin {
                plugin: plugin.plugin.clone(),
            },
        };
        found.extend(reader.load(&root, Some(&plugin.plugin)).await);
    }
    found
}

fn text<'a>(map: &'a std::collections::BTreeMap<String, Value>, key: &str) -> Option<&'a str> {
    map.get(key).and_then(Value::text)
}

fn boolean(map: &std::collections::BTreeMap<String, Value>, key: &str) -> bool {
    text(map, key).is_some_and(|value| value.trim() == "true")
}

/// 没有 `description` 时取正文第一个非空行。
fn first_line(body: &str) -> String {
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let heading = line
            .strip_prefix('#')
            .map(|rest| rest.trim_start_matches('#'))
            .filter(|rest| rest.starts_with([' ', '\t']))
            .map(str::trim)
            .filter(|rest| !rest.is_empty());
        let text = heading.unwrap_or(line);
        return if text.chars().count() > 100 {
            format!("{}...", text.chars().take(97).collect::<String>())
        } else {
            text.to_owned()
        };
    }
    "Skill".to_owned()
}

/// `allowed-tools` 一类的工具列表：逗号或空白分隔，括号里的不切。
pub(crate) fn tool_list(value: Option<&Value>) -> Vec<String> {
    let items: Vec<&str> = match value {
        Some(Value::Text(text)) => vec![text.as_str()],
        Some(Value::List(items)) => items.iter().map(String::as_str).collect(),
        None => Vec::new(),
    };
    let mut out = Vec::new();
    for item in items {
        let mut depth = 0usize;
        let mut current = String::new();
        for c in item.chars() {
            match c {
                '(' => depth += 1,
                ')' => depth = depth.saturating_sub(1),
                _ => {}
            }
            if depth == 0 && (c == ',' || c.is_whitespace()) {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
                continue;
            }
            current.push(c);
        }
        if !current.is_empty() {
            out.push(current);
        }
    }
    out
}

/// 命名参数：去掉空串与纯数字。
fn arg_names(value: Option<&Value>) -> Vec<String> {
    let items: Vec<String> = match value {
        Some(Value::Text(text)) => text.split_whitespace().map(str::to_owned).collect(),
        Some(Value::List(items)) => items.clone(),
        None => Vec::new(),
    };
    items
        .into_iter()
        .filter(|item| !item.trim().is_empty() && !item.chars().all(|c| c.is_ascii_digit()))
        .collect()
}

/// `paths`：`/**` 结尾去掉，全是 `**` 时视为无条件。
fn skill_paths(value: Option<&Value>) -> Option<Vec<String>> {
    let items: Vec<String> = match value? {
        Value::Text(text) => text.split(',').map(|item| item.trim().to_owned()).collect(),
        Value::List(items) => items.clone(),
    };
    let patterns: Vec<String> = items
        .into_iter()
        .map(|pattern| match pattern.strip_suffix("/**") {
            Some(rest) => rest.to_owned(),
            None => pattern,
        })
        .filter(|pattern| !pattern.is_empty())
        .collect();
    (!patterns.is_empty() && !patterns.iter().all(|pattern| pattern == "**")).then_some(patterns)
}

/// 一个 SKILL.md；没有 frontmatter 时整份都是正文。
pub(crate) fn parse(raw: &str, name: String, dir: &str, source: SkillSource) -> Skill {
    let (map, body) =
        frontmatter::split(raw).unwrap_or_else(|| (Default::default(), raw.to_owned()));
    let description = text(&map, "description")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| first_line(&body));
    let model = text(&map, "model")
        .map(str::trim)
        .filter(|model| !model.is_empty() && !model.eq_ignore_ascii_case("inherit"))
        .map(str::to_owned);
    Skill {
        name,
        display: text(&map, "name").map(str::to_owned),
        description,
        when_to_use: text(&map, "when_to_use").map(str::to_owned),
        argument_hint: text(&map, "argument-hint").map(str::to_owned),
        arg_names: arg_names(map.get("arguments")),
        allowed_tools: tool_list(map.get("allowed-tools")),
        disallowed_tools: tool_list(map.get("disallowed-tools")),
        disable_model_invocation: boolean(&map, "disable-model-invocation"),
        user_invocable: text(&map, "user-invocable").is_none_or(|value| value.trim() == "true"),
        paths: skill_paths(map.get("paths")),
        shell: match text(&map, "shell").map(str::trim) {
            Some("powershell") => SkillShell::PowerShell,
            _ => SkillShell::Bash,
        },
        has_hooks: map.contains_key("hooks"),
        model,
        effort: text(&map, "effort").map(str::to_owned),
        fork: text(&map, "context").map(str::trim) == Some("fork"),
        agent: text(&map, "agent").map(str::to_owned),
        dir: dir.to_owned(),
        body,
        source,
    }
}

#[cfg(test)]
#[path = "load_tests.rs"]
mod tests;
