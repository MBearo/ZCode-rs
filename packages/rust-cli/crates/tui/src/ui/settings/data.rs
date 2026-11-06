//! 设置页读到的段（docs/specs/rust-cli/tui-kit.md §4.2「设置」；照 Puffin `host/projection/settings.rs` 的形状）：runner
//! 打开一段时按旧界面的服务与内核读出来交给页（`runner/settings.rs`），页是它们的纯函数。Appearance 与 Machines 不经这里：
//! 样子在 `app.ink.choice`，机器在 `app.machines`。
//!
//! Models 的内置家族来自账号端口（`Account::families`）：它不知道的家族不列（cli 的 Puffin host 也不给）。
use std::collections::BTreeMap;

use rust_cli_kernel_api::permission::{RuleBehavior, RuleSource, SourcedRule};

/// 经 runner 读的段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Section {
    General,
    Permissions,
    Models,
    Memory,
    Subagents,
    Mcp,
    Skills,
    Commands,
    Hooks,
}

impl Section {
    pub const ALL: [Section; 9] = [
        Section::General,
        Section::Permissions,
        Section::Models,
        Section::Memory,
        Section::Subagents,
        Section::Mcp,
        Section::Skills,
        Section::Commands,
        Section::Hooks,
    ];

    /// 段的 id（kit `SECTIONS` 的 id）。
    pub fn id(self) -> &'static str {
        match self {
            Section::General => "general",
            Section::Permissions => "permissions",
            Section::Models => "models",
            Section::Memory => "memory",
            Section::Subagents => "subagents",
            Section::Mcp => "mcp",
            Section::Skills => "skills",
            Section::Commands => "commands",
            Section::Hooks => "hooks",
        }
    }

    /// id 为 `id` 的、经 runner 读的段；Appearance 与 Machines 不是。
    pub fn of(id: &str) -> Option<Section> {
        Section::ALL.into_iter().find(|s| s.id() == id)
    }
}

/// 一段的数据。
#[derive(Debug, Clone, PartialEq)]
pub enum SectionData {
    General(General),
    Permissions(Permissions),
    Models(Models),
    Memory(Vec<MemoryFile>),
    Subagents(Vec<Subagent>),
    Mcp(Vec<McpServer>),
    Skills(Vec<Skill>),
    Commands(Vec<CommandItem>),
    Hooks(Vec<Hook>),
}

impl SectionData {
    pub fn section(&self) -> Section {
        match self {
            SectionData::General(_) => Section::General,
            SectionData::Permissions(_) => Section::Permissions,
            SectionData::Models(_) => Section::Models,
            SectionData::Memory(_) => Section::Memory,
            SectionData::Subagents(_) => Section::Subagents,
            SectionData::Mcp(_) => Section::Mcp,
            SectionData::Skills(_) => Section::Skills,
            SectionData::Commands(_) => Section::Commands,
            SectionData::Hooks(_) => Section::Hooks,
        }
    }
}

/// 读到的各段，按段。
pub type Data = BTreeMap<Section, SectionData>;

/// General：自动压缩的位置（外层 `None` 是读不到配置，内层 `None` 是没设、用模型上限）与界面语言（`auto`、`en-US`、
/// `zh-CN`）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct General {
    pub auto_compact: Option<Option<u64>>,
    pub locale: String,
}

/// Permissions：内核列出的规则、配置文件来源的规则来自的文件、工作的文件夹。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Permissions {
    pub rules: Vec<SourcedRule>,
    pub files: Vec<RuleFile>,
    pub folders: Vec<Folder>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleFile {
    pub source: RuleSource,
    pub behavior: RuleBehavior,
    pub path: String,
}

/// 工作的文件夹：工作区根，或 `--add-dir` 加的（`added`）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Folder {
    pub path: String,
    pub added: bool,
}

/// 新会话的模型：（provider，模型，档位）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pick {
    pub provider: String,
    pub model: String,
    pub effort: String,
}

/// 模型目录里的一个 provider：名字与它的模型（Puffin `ProviderSettings` 的这几项；目录里的都开着，限额随目录）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub models: Vec<FamilyModel>,
}

/// Models：缺省模型、模型目录里的 provider（只读，同 Puffin 的 host：provider 的改动这一版还没有）与缺省模型的选择器列的
/// 模型目录。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Models {
    pub default_model: Option<Pick>,
    /// 内置的家族（Z.ai、BigModel）：登录状态与它们的模型（Puffin `ModelsSettings.families`）。
    pub families: Vec<crate::services::FamilyStatus>,
    pub providers: Vec<Provider>,
    pub catalog: Vec<rust_cli_kernel_api::provider::CatalogModel>,
}

pub use crate::services::FamilyModel;

/// 一个规则文件：范围（内核的叫法原样）、路径、在不在与大小。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryFile {
    pub scope: String,
    pub path: String,
    pub exists: bool,
    pub size: u64,
}

/// 清单里一项的来源。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Scope {
    #[default]
    User,
    Project,
    Plugin,
    Builtin,
}

impl Scope {
    /// 来源的名字：`builtin` 写成 `built in`（Puffin `scope_name`）。
    pub fn name(self) -> &'static str {
        match self {
            Scope::User => "user",
            Scope::Project => "project",
            Scope::Plugin => "plugin",
            Scope::Builtin => "built in",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Subagent {
    pub name: String,
    pub description: String,
    pub scope: Scope,
    /// 定义它的文件；内置的为 ""。
    pub path: String,
    /// None 是全部工具。
    pub tools: Option<Vec<String>>,
    /// None 是继承会话的模型。
    pub model: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum McpKind {
    #[default]
    Stdio,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum McpState {
    #[default]
    Connecting,
    Connected,
    Disabled,
    Failed,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpServer {
    pub name: String,
    pub scope: Scope,
    pub enabled: bool,
    pub kind: McpKind,
    /// stdio 的命令行，或 HTTP / SSE 的地址；内核的快照没有时为 ""。
    pub target: String,
    pub state: McpState,
    pub tools: usize,
    pub error: String,
    pub machine: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub scope: Scope,
    pub path: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandItem {
    pub name: String,
    pub description: String,
    pub argument_hint: String,
    pub scope: Scope,
    pub path: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hook {
    pub event: String,
    pub matcher: String,
    pub command: String,
    pub timeout: Option<u64>,
    pub scope: Scope,
    /// 定义它的文件或来源。
    pub source: String,
}

/// hook 与配置项的来源 → 范围（`user`、`project`、`local`、`policy`、`plugin:<名字>`；Puffin host 的 `scope_of`）。
pub fn scope_of(source: &str) -> Scope {
    match source {
        "project" | "local" => Scope::Project,
        "policy" => Scope::Builtin,
        source if source.starts_with("plugin") => Scope::Plugin,
        _ => Scope::User,
    }
}
