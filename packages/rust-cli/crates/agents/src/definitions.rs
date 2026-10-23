//! agent 定义：内置 agent、清单行与工具说明（docs/specs/rust-cli/agents.md §3）。
//!
//! 内置 agent 有 general-purpose、Explore、Plan；清单行与工具说明见 `tools_description`。
//! 只描述别的产品功能的内置 agent 不提供。

use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_prompts::agents as texts;

pub(crate) const GENERAL_PURPOSE: &str = "general-purpose";
pub(crate) const EXPLORE: &str = "Explore";
pub(crate) const PLAN: &str = "Plan";

/// Explore / Plan 不能用的工具（Agent、Artifact 一组工具、ExitPlanMode 与
/// 改文件的工具）。名字原样保留，清单行逐字照写。
const READ_ONLY_DISALLOWED: &[&str] = &[
    "Agent",
    "Artifact",
    "ArtifactComments",
    "ArtifactData",
    "ArtifactCheck",
    "ExitPlanMode",
    "Edit",
    "Write",
    "NotebookEdit",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentSource {
    BuiltIn,
    /// `--plugin-dir` 插件提供（agents.md §3）。
    Plugin,
    User,
    Project,
}

/// 一个 agent 类型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDefinition {
    pub agent_type: String,
    /// 清单里的描述（Explore 用 lean 描述）。
    pub when_to_use: String,
    /// None：全部工具；`["*"]` 同样是全部，清单里写作 `*`。
    pub tools: Option<Vec<String>>,
    pub disallowed_tools: Vec<String>,
    /// system prompt 的第一部分。
    pub prompt: String,
    pub source: AgentSource,
    /// `inherit` 或缺省时继承父会话的模型；其余是模型 ID。
    pub model: Option<String>,
    pub effort: Option<String>,
    pub max_turns: Option<u32>,
    pub permission_mode: Option<PermissionMode>,
    /// frontmatter `background: true`：总在后台运行。
    pub background: bool,
}

impl AgentDefinition {
    /// 一次性的内置 agent：结果不带 agentId 与用量尾注。
    pub fn one_shot(&self) -> bool {
        self.source == AgentSource::BuiltIn
            && (self.agent_type == EXPLORE || self.agent_type == PLAN)
    }

    /// 不带规则文件与 git 状态（按类型名）。
    pub fn omits_context(&self) -> bool {
        self.agent_type == EXPLORE || self.agent_type == PLAN
    }

    /// 清单行（`agent_listing_delta` 的 `addedLines`）。
    pub fn listing_line(&self) -> String {
        format!(
            "- {}: {} (Tools: {})",
            self.agent_type,
            self.when_to_use,
            tools_description(self.tools.as_deref(), &self.disallowed_tools)
        )
    }
}

/// agent 清单行里的工具说明：允许的工具去掉禁止的；没有清单时是 `All tools` 或 `All tools except …`。
pub(crate) fn tools_description(tools: Option<&[String]>, disallowed: &[String]) -> String {
    let allow = tools.filter(|tools| !tools.is_empty());
    match (allow, disallowed.is_empty()) {
        (Some(tools), false) => {
            let left: Vec<&str> = tools
                .iter()
                .filter(|tool| !disallowed.contains(tool))
                .map(String::as_str)
                .collect();
            if left.is_empty() {
                "None".to_owned()
            } else {
                left.join(", ")
            }
        }
        (Some(tools), true) => tools.join(", "),
        (None, false) => format!("All tools except {}", disallowed.join(", ")),
        (None, true) => "All tools".to_owned(),
    }
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| (*name).to_owned()).collect()
}

fn built_in(agent_type: &str, when_to_use: &str, prompt: &str) -> AgentDefinition {
    AgentDefinition {
        agent_type: agent_type.to_owned(),
        when_to_use: when_to_use.to_owned(),
        tools: None,
        disallowed_tools: Vec::new(),
        prompt: prompt.to_owned(),
        source: AgentSource::BuiltIn,
        model: None,
        effort: None,
        max_turns: None,
        permission_mode: None,
        background: false,
    }
}

/// 内置 agent，按注册顺序。
pub fn builtins() -> Vec<AgentDefinition> {
    let general = AgentDefinition {
        tools: Some(names(&["*"])),
        ..built_in(
            GENERAL_PURPOSE,
            texts::when_general_purpose(),
            texts::general_purpose(),
        )
    };
    let read_only = |agent_type, when_to_use, prompt| AgentDefinition {
        disallowed_tools: names(READ_ONLY_DISALLOWED),
        model: Some("inherit".to_owned()),
        ..built_in(agent_type, when_to_use, prompt)
    };
    vec![
        general,
        read_only(EXPLORE, texts::when_explore(), texts::explore()),
        read_only(PLAN, texts::when_plan(), texts::plan()),
    ]
}

/// 合并各来源：同名时后者覆盖前者（内置 < 插件 < 用户 < 项目），保留首次
/// 出现的位置。
pub(crate) fn merge(layers: Vec<Vec<AgentDefinition>>) -> Vec<AgentDefinition> {
    let mut merged: Vec<AgentDefinition> = Vec::new();
    for definition in layers.into_iter().flatten() {
        match merged
            .iter_mut()
            .find(|known| known.agent_type == definition.agent_type)
        {
            Some(known) => *known = definition,
            None => merged.push(definition),
        }
    }
    merged
}

#[cfg(test)]
#[path = "definitions_tests.rs"]
mod tests;
