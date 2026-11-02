//! 权限模式（docs/specs/rust-cli/README.md §5）。
//!
//! 取值是 `--permission-mode` 的模式加上 Node 的 guarded。落库时映射到 Node 已有的模式值：default → build、
//! acceptEdits → edit、bypassPermissions → yolo、guarded → guarded、auto → auto；dontAsk 记为 build 并另写一条
//! `rust-cli/` entry（映射在会话存储适配器里做）。plan 是独立开关（[`ExecutionState`]，修复线 F20）。

use serde::{Deserialize, Serialize};

pub mod syntax;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionMode {
    Default,
    AcceptEdits,
    /// 计划模式。修复线 F20 起 plan 是独立开关（[`ExecutionState::plan`]）：这个值只出现在“判决用的模式”
    /// （[`ExecutionState::effective`]）与旧的单值写法里（读成“plan 开、权限模式不变”）。
    Plan,
    BypassPermissions,
    DontAsk,
    Auto,
    /// Node 的 guarded（修复线 F20）：同 bypassPermissions，Bash 命令命中危险命令规则时先询问一次
    /// （docs/specs/rust-cli/shell-permissions.md §4.6.1）。命令行不收这个模式。
    Guarded,
}

/// 模式的名字只定义在这里（修复线 F58，README §4.1）：Node 名（会话行、配置、`/mode`）与命令行名（命令行、agent 的
/// frontmatter、stream-json、`rust-cli/permission_mode` entry）。各入口收的集合不同，照各自的来源再过滤。
impl PermissionMode {
    /// 命令行的名字（serde 的写法）；guarded 是本工程的 `guarded`。
    pub fn cli_name(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::AcceptEdits => "acceptEdits",
            Self::Plan => "plan",
            Self::BypassPermissions => "bypassPermissions",
            Self::DontAsk => "dontAsk",
            Self::Auto => "auto",
            Self::Guarded => "guarded",
        }
    }

    /// Node 的名字（README §5 的映射：default 与 dontAsk 都记 `build`）。
    pub fn node_name(self) -> &'static str {
        match self {
            Self::Default | Self::DontAsk => "build",
            Self::AcceptEdits => "edit",
            Self::Plan => "plan",
            Self::BypassPermissions => "yolo",
            Self::Guarded => "guarded",
            Self::Auto => "auto",
        }
    }

    /// Node 的模式值：`plan`、`build`、`edit`、`yolo`、`guarded`、`auto`；其余为 None。
    pub fn from_node_name(name: &str) -> Option<Self> {
        Some(match name {
            "plan" => Self::Plan,
            "build" => Self::Default,
            "edit" => Self::AcceptEdits,
            "yolo" => Self::BypassPermissions,
            "guarded" => Self::Guarded,
            "auto" => Self::Auto,
            _ => return None,
        })
    }

    /// 命令行收的模式名（`--permission-mode` 与 agent 的 `permissionMode`）：命令行帮助列出的 6 个，`manual` 是 `default`
    /// 的别名；不收 guarded。
    pub fn from_cli_choice(name: &str) -> Option<Self> {
        Some(match name {
            "default" | "manual" => Self::Default,
            "acceptEdits" => Self::AcceptEdits,
            "plan" => Self::Plan,
            "bypassPermissions" => Self::BypassPermissions,
            "dontAsk" => Self::DontAsk,
            "auto" => Self::Auto,
            _ => return None,
        })
    }
}

/// 会话的执行状态：权限模式与 plan 开关（Node `ExecutionState{mode, planEnabled}`，修复线 F20，
/// docs/specs/rust-cli/shell-permissions.md §4.6）。两个值互相独立：改权限模式不动 plan，开关 plan 不动权限模式。
/// 会话 actor 是唯一 owner。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExecutionState {
    /// 权限模式，不是 [`PermissionMode::Plan`]。
    pub mode: PermissionMode,
    #[serde(default)]
    pub plan: bool,
}

impl Default for ExecutionState {
    fn default() -> Self {
        Self {
            mode: PermissionMode::Default,
            plan: false,
        }
    }
}

impl ExecutionState {
    /// 判决、工具与上下文看的模式：plan 开时是 plan，否则是权限模式。
    pub fn effective(self) -> PermissionMode {
        if self.plan {
            PermissionMode::Plan
        } else {
            self.mode
        }
    }

    /// 换权限模式；`Plan`（旧写法）只打开 plan、模式不变（Node `resolveExecutionState` 的 `mode: plan`）。
    /// 其余值不动 plan。
    #[must_use]
    pub fn with_mode(self, mode: PermissionMode) -> Self {
        match mode {
            PermissionMode::Plan => Self { plan: true, ..self },
            mode => Self { mode, ..self },
        }
    }

    #[must_use]
    pub fn with_plan(self, plan: bool) -> Self {
        Self { plan, ..self }
    }

    /// 由单值模式与另记的 plan 开关组成（`CreateSession`、`LoadedSession`、`SessionFork` 的两个字段）：
    /// `mode` 是旧写法的 `Plan` 时同样算 plan 开。
    pub fn from_parts(mode: PermissionMode, plan: bool) -> Self {
        let state = Self::from(mode);
        Self {
            plan: state.plan || plan,
            ..state
        }
    }
}

/// 旧的单值写法（`CreateSession.mode`、`--permission-mode`、配置）：`Plan` 是“plan 开、default”。
impl From<PermissionMode> for ExecutionState {
    fn from(mode: PermissionMode) -> Self {
        Self::default().with_mode(mode)
    }
}

/// 一条权限规则，例如 `Bash(git status:*)`、`Read(//etc/**)`、`WebFetch(domain:example.com)`。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PermissionRule {
    pub tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleBehavior {
    Allow,
    Deny,
    Ask,
}

/// 一次权限判定的结果（工具自己的 `check_permissions` 与判决链都返回它）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "behavior", rename_all = "snake_case")]
pub enum PermissionDecision {
    /// 允许；`updated_input` 不为空时用它替换原输入（替换后仍要重新校验 schema）。
    Allow {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        updated_input: Option<serde_json::Value>,
    },
    /// 询问用户；`suggestions` 是可一键加入的规则。普通的询问用 [`PermissionDecision::ask`] 建。
    Ask {
        message: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        suggestions: Vec<PermissionRule>,
        /// 只批准这一次（guarded 的危险命令，Node `approvalMode: "user-once"`，docs/specs/rust-cli/shell-permissions.md
        /// §4.6.1）：执行管线不跑 PermissionRequest hooks，hook 不能替用户批准。只有判决链给出。
        ///
        /// 工作线 F137（审查 T6 的剩余）：以前是 `PermissionGate::ask_once`，执行管线询问前另调一次，它又向执行环境取一次
        /// 路径风格（远端执行时多一个往返）；现在是判决结果的一部分，一次判决只取一次路径事实。
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        once: bool,
    },
    Deny {
        message: String,
    },
    /// 工具没有意见，交给通用判决链。
    Passthrough,
}

impl PermissionDecision {
    /// 普通的询问（不是“只批准这一次”）。
    pub fn ask(message: impl Into<String>, suggestions: Vec<PermissionRule>) -> Self {
        Self::Ask {
            message: message.into(),
            suggestions,
            once: false,
        }
    }
}

/// 规则从哪里来（zcode 的配置来源）。按这个顺序列出，
/// 询问时的“规则来源”说明也用它（docs/specs/rust-cli/shell-permissions.md §5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuleSource {
    /// 用户配置（`~/.zcode` 下的配置文件，`permission.allowedTools` 等）。
    UserSettings,
    /// 项目配置（工作区 `.zcode` 下的配置文件）。
    ProjectSettings,
    /// 本机的项目规则集（Node `local_setting` 的 `permission/ruleset`，“总是允许”写到这里）。
    LocalSettings,
    /// 命令行参数（`--allowedTools` / `--disallowedTools`）。
    CliArg,
    /// 本会话内的规则（只在内存里）。
    Session,
}

/// 带来源与行为的一条规则。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourcedRule {
    pub source: RuleSource,
    pub behavior: RuleBehavior,
    pub rule: PermissionRule,
}

/// 文件访问的方向：读按 `Read(...)` 规则，写按 `Edit(...)` 规则（Write、NotebookEdit 共用；shell-permissions.md §4.4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAccess {
    Read,
    Write,
}

impl FileAccess {
    /// 这个方向的路径规则写在哪个工具名下。
    pub fn rule_tool(self) -> &'static str {
        match self {
            Self::Read => crate::tool_names::READ,
            Self::Write => crate::tool_names::EDIT,
        }
    }
}

/// 内置权限检查的判决主体（shell-permissions.md §4.1）：工具声明这次输入按哪一类检查（[`crate::tool::Tool::permission_subject`]）。
/// 没有主体的工具只经整个工具的规则与它自己的 `check_permissions`。
///
/// F125（T3）：以前判决链按工具名分派（`Bash`、带 `command` 的 `Monitor`、六个文件工具、`WebFetch`、`Skill`），哪个工具
/// 用哪类检查、路径在哪个字段，都写在 policy 里。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionSubject {
    /// 一条 shell 命令：按 `Bash(...)` 规则、只读判定、危险删除与复合结构判。
    Shell { command: String },
    /// 一个文件路径；`path` 为 None 时（Glob、Grep 没给路径）是当前目录。
    Path {
        access: FileAccess,
        path: Option<String>,
    },
    /// 一个网址：按 `WebFetch(domain:<host>)` 规则与预批准域名判。
    Url { url: String },
    /// 一个 skill：按 `Skill(<名字>)` 规则判；`name` 是模型给的原文。
    Skill { name: String },
}

/// 判决链用的规则快照（README §6 规则 7：带版本的快照值）。由入口层或会话从配置组装后交给
/// 判决链；判决链自己不读配置。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRules {
    pub version: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<SourcedRule>,
    /// 工作目录以外、也算“工作目录”的目录。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_directories: Vec<crate::env::EnvPath>,
    /// 没有人能回答询问（例如后台子代理）：询问直接按拒绝
    /// 处理，拒绝的文本就是询问的文本。
    #[serde(default)]
    pub avoid_prompts: bool,
}

impl PermissionRules {
    /// 针对整个工具（没有 content）、行为是 `behavior` 的第一条规则：工具名相同，或 MCP 服务器规则
    /// （[`syntax::tool_matches`]）。判决链与工具清单（整个工具被 deny 时从清单去掉，tools-files.md §1.1.1）共用。
    pub fn whole_tool(&self, tool: &str, behavior: RuleBehavior) -> Option<&SourcedRule> {
        self.rules.iter().find(|sourced| {
            sourced.behavior == behavior
                && sourced.rule.content.is_none()
                && syntax::tool_matches(&sourced.rule.tool, tool)
        })
    }
}

/// 本机项目规则集（Node `local_setting(namespace=permission, key=ruleset, scope=project)` 的 `allow` /
/// `deny` / `ask`，规则值 `{toolName, ruleContent?}`），docs/specs/rust-cli/shell-permissions.md §5。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectRules {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<PermissionRule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny: Vec<PermissionRule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ask: Vec<PermissionRule>,
}

impl ProjectRules {
    /// 作为 LocalSettings 来源的规则。
    pub fn sourced(&self) -> Vec<SourcedRule> {
        [
            (RuleBehavior::Allow, &self.allow),
            (RuleBehavior::Deny, &self.deny),
            (RuleBehavior::Ask, &self.ask),
        ]
        .into_iter()
        .flat_map(|(behavior, rules)| {
            rules.iter().map(move |rule| SourcedRule {
                source: RuleSource::LocalSettings,
                behavior,
                rule: rule.clone(),
            })
        })
        .collect()
    }
}

/// `/permissions` 的一次修改（docs/specs/rust-cli/tui-manage.md §1.1）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuleChange {
    /// 把规则加进本机项目规则集（LocalSettings）。`rules` 是规则文字（`Bash(ls *)`），内核按
    /// 规则语法解析。
    Add {
        behavior: RuleBehavior,
        rules: Vec<String>,
    },
    /// 删除列表里的一条规则：LocalSettings 落库，CliArg 与 Session 只改内存；配置文件来源只读，被拒绝。
    Remove { rule: SourcedRule },
}

/// `/permissions` 的列表查询：工作区决定本机项目规则集，有会话时另列本会话的规则。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRulesQuery {
    pub workspace: crate::env::WorkspaceRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<crate::ids::SessionId>,
}

/// `/permissions` 的修改请求；回执是修改后的列表。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRulesChange {
    pub workspace: crate::env::WorkspaceRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<crate::ids::SessionId>,
    pub change: RuleChange,
}

#[cfg(test)]
#[path = "permission_tests.rs"]
mod tests;
