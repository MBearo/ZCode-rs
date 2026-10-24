//! skill（docs/specs/rust-cli/ext.md §2）：发现、frontmatter、清单、Skill 工具。
//!
//! 文件经执行环境端口读；解析结果按 SKILL.md 的路径与 `FileVersion` 缓存，每一步重新列目录、
//! `stat` 每个 SKILL.md，版本变了才重读（相当于文件监视 + memoize）。

mod args;
mod body;
mod builtin;
mod closest;
mod frontmatter;
mod inline;
mod invoked;
mod listing;
mod load;
mod mentions;
mod names;
mod paths;
mod tool;
mod unknown;
mod user;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use rust_cli_kernel_api::env::{EnvFacts, FileVersion, WorkspaceRef};
use rust_cli_kernel_api::listing::ListingEntry;
use rust_cli_kernel_api::machine::{EnvInfo, Files};
use rust_cli_kernel_api::message::Message;

pub use invoked::INVOKED_SKILLS_SOURCE;
pub use tool::{SKILL_MESSAGE_SOURCE, SkillTool};
pub use user::SkillCommands;

/// `!` 命令的执行者：注册表里的 Bash 工具、判决链与 ID（ext.md §2.5）。
pub(crate) fn runner(
    bash: Arc<dyn rust_cli_kernel_api::tool::Tool>,
    gate: Arc<rust_cli_policy::PolicyGate>,
    ids: Arc<dyn rust_cli_kernel_api::clock::Ids>,
) -> inline::Runner {
    inline::Runner { bash, gate, ids }
}

/// skill 从哪里来（`source`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillSource {
    Managed,
    User,
    Project,
    Plugin {
        plugin: String,
    },
    /// 内置（bundled）：只有 `/loop`（tools-more.md §17.1）。
    Builtin,
}

/// `!` 命令用的 shell（frontmatter `shell`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SkillShell {
    #[default]
    Bash,
    PowerShell,
}

/// 一个 skill（解析结果里本工程用到的部分）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    /// 调用名：目录名；插件 skill 为 `<插件名>:<目录名>`。
    pub name: String,
    /// frontmatter 的 `name`（面向用户的名字）。
    pub display: Option<String>,
    pub description: String,
    pub when_to_use: Option<String>,
    pub argument_hint: Option<String>,
    pub arg_names: Vec<String>,
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    pub disable_model_invocation: bool,
    pub user_invocable: bool,
    pub paths: Option<Vec<String>>,
    pub shell: SkillShell,
    pub has_hooks: bool,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub fork: bool,
    pub agent: Option<String>,
    /// skill 目录（执行环境里的路径）。
    pub dir: String,
    /// frontmatter 之后的正文，原样。
    pub body: String,
    pub source: SkillSource,
}

impl Skill {
    /// 只有安全字段：没有 `allowed-tools`、`disallowed-tools`、`hooks`。这样的 skill
    /// 不经询问即可调用（ext.md §2.4）。
    pub fn is_safe(&self) -> bool {
        // 内置的 `/loop` 不算安全：bundled skill 带 `argsMayContainSlashCommands` 等不在安全名单里的
        // 字段，所以要询问，dontAsk 下拒绝。
        self.source != SkillSource::Builtin
            && self.allowed_tools.is_empty()
            && self.disallowed_tools.is_empty()
            && !self.has_hooks
    }
}

/// 插件提供的 skill 根目录（ext.md §4）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSkillRoot {
    pub plugin: String,
    /// `<插件>/skills` 一类的目录。
    pub dir: String,
}

#[derive(Default)]
struct Cache {
    /// SKILL.md 路径 → 解析时的版本与结果。
    parsed: HashMap<String, (FileVersion, Arc<Skill>)>,
}

/// skill 目录的发现与缓存。
pub struct SkillCatalog {
    /// 环境事实（skill 目录的层级、路径风格）与 skill 文件；都是本机的。
    info: Arc<dyn EnvInfo>,
    files: Arc<dyn Files>,
    plugins: Mutex<Vec<PluginSkillRoot>>,
    cache: Mutex<Cache>,
    /// 单次的 `-p`（`singleShotPrintSession`）：内置 `/loop` 的 Monitor 超时随它（tools-more.md §17.1）。
    print: AtomicBool,
}

impl SkillCatalog {
    pub fn new(info: Arc<dyn EnvInfo>, files: Arc<dyn Files>) -> Self {
        Self {
            info,
            files,
            plugins: Mutex::default(),
            cache: Mutex::default(),
            print: AtomicBool::new(false),
        }
    }

    /// 内核按形态设置：`-p` 是单次的打印会话。
    pub fn set_print_session(&self, print: bool) {
        self.print.store(print, Ordering::Relaxed);
    }

    pub(crate) fn print_session(&self) -> bool {
        self.print.load(Ordering::Relaxed)
    }

    /// 换上插件的 skill 根目录（插件加载完成后，ext.md §4）。
    pub fn set_plugin_roots(&self, roots: Vec<PluginSkillRoot>) {
        *self.plugins.lock().unwrap_or_else(PoisonError::into_inner) = roots;
    }

    fn plugin_roots(&self) -> Vec<PluginSkillRoot> {
        self.plugins
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn cached(&self, path: &str, version: &FileVersion) -> Option<Arc<Skill>> {
        let cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        cache
            .parsed
            .get(path)
            .filter(|(known, _)| known == version)
            .map(|(_, skill)| skill.clone())
    }

    pub(crate) fn remember(&self, path: String, version: FileVersion, skill: Arc<Skill>) {
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        cache.parsed.insert(path, (version, skill));
    }

    /// 这一步发现的全部 skill，按优先级排好、同名只留第一个（ext.md §2.1）。
    pub async fn skills(&self, workspace: &WorkspaceRef) -> Vec<Arc<Skill>> {
        let facts = match self.info.facts(workspace).await {
            Ok(facts) => facts,
            Err(error) => {
                tracing::debug!(%error, "skills are not available without environment facts");
                return Vec::new();
            }
        };
        self.skills_with(workspace, &facts).await
    }

    async fn skills_with(&self, workspace: &WorkspaceRef, facts: &EnvFacts) -> Vec<Arc<Skill>> {
        let mut found = load::discover(self, workspace, facts, &self.plugin_roots()).await;
        // 内置 skill 排在文件与插件 skill 之后，同名时前面的生效。
        found.extend(builtin::skills());
        let mut seen = HashSet::new();
        found
            .into_iter()
            .filter(|skill| seen.insert(skill.name.clone()))
            .collect()
    }

    /// 这次会话调用过的 skill（compact.md §3.4）：`history` 是全部历史，最近的在前。
    pub async fn invoked(
        &self,
        workspace: &WorkspaceRef,
        history: &[Message],
    ) -> Vec<rust_cli_kernel_api::listing::InvokedSkill> {
        invoked::invoked(self, workspace, history).await
    }

    /// 按名字找（Skill 工具用）。
    pub async fn find(&self, workspace: &WorkspaceRef, name: &str) -> Option<Arc<Skill>> {
        self.skills(workspace)
            .await
            .into_iter()
            .find(|skill| skill.name == name)
    }

    /// 这一步给模型的 skill 清单（ext.md §2.3）：去掉禁止模型调用的与未激活的条件 skill。预算按 `model` 的上下文
    /// 窗口（模型卡片，没有时 200000）与每 token 字符数（没有模型时 4）。
    pub async fn listing(
        &self,
        workspace: &WorkspaceRef,
        history: &[Message],
        model: &str,
    ) -> Vec<ListingEntry> {
        let Ok(facts) = self.info.facts(workspace).await else {
            return Vec::new();
        };
        let skills = self.skills_with(workspace, &facts).await;
        let touched = paths::touched(history, facts.cwd.as_str());
        let visible: Vec<Arc<Skill>> = skills
            .iter()
            .filter(|skill| !skill.disable_model_invocation)
            .filter(|skill| paths::active(skill, &touched))
            .cloned()
            .collect();
        let window = rust_cli_prompts::models::card(model).map(|card| card.context_window);
        let chars = rust_cli_prompts::models::chars_per_token(model);
        listing::entries(&visible, &skills, listing::budget(window, chars))
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
