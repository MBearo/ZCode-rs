//! 上下文用量（`/context`，docs/specs/rust-cli/context-usage.md）：从会话视图、主循环的工具注册表、上下文
//! 分析与压缩的窗口规则现算，计数经模型端口（`counts`），算术在 `arith`。只读，不改会话。

mod arith;
mod counts;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use futures_util::future::join_all;
use rust_cli_agents::{AgentCatalog, AgentSource};
use rust_cli_compact::CompactConfig;
use rust_cli_compact::window::{WindowSource as Source, context_buffer, window};
use rust_cli_context::PromptAssembler;
use rust_cli_ext::skills::{SkillCatalog, SkillSource};
use rust_cli_kernel_api::context_usage::{
    AgentUsage, ContextUsage, ContextUsageQuery, MemoryFileUsage, SkillUsage, WindowSource,
};
use rust_cli_kernel_api::history;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::mcp_state::WAIT_FOR_MCP_SERVERS;
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::model::{ModelPort, QuerySource, ToolDefinition};
use rust_cli_kernel_api::orchestration::{ContextRequest, ToolRegistry};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::KernelError;
use rust_cli_kernel_api::tool::{PromptContext, Tool};
use rust_cli_kernel_api::tool_search::{TOOL_SEARCH, compare_names, is_deferred, referenced};

use self::arith::{McpTool, Measured, Messages};
pub(crate) use self::counts::Cache;
use self::counts::{Counter, TOOL_OVERHEAD, rough};

/// Skill 工具的名字。
const SKILL_TOOL: &str = "Skill";

/// 分析要用的能力（runtime 组装时给出，与主循环共用同一批实例）。
pub(crate) struct UsageAnalyzer {
    pub(crate) model: Arc<dyn ModelPort>,
    pub(crate) context: Arc<PromptAssembler>,
    pub(crate) tools: Arc<dyn ToolRegistry>,
    pub(crate) agents: Arc<AgentCatalog>,
    pub(crate) skills: Arc<SkillCatalog>,
    pub(crate) compact: CompactConfig,
    pub(crate) cache: Cache,
}

/// 主循环发给模型的工具，按 `/context` 的计数分组（§3：d、e、a、f）。
#[derive(Default)]
struct Wired {
    listed: Vec<ToolDefinition>,
    deferred: Vec<ToolDefinition>,
    mcp: Vec<(ToolDefinition, bool)>,
    skill: Option<ToolDefinition>,
    search: bool,
}

/// 一组工具的定义：描述按“这一组”作为可见工具生成——计数的子集当作 `tools` 传给每个
/// 工具的 prompt，所以 Agent / ListAgents 在列出的那一组里看不到延迟的 SendMessage（context-usage.md §3）。
fn definitions(group: &[&Arc<dyn Tool>], mode: PermissionMode) -> Vec<ToolDefinition> {
    let prompt = PromptContext {
        tool_names: group.iter().map(|tool| tool.name().to_owned()).collect(),
        mode,
    };
    group
        .iter()
        .map(|tool| ToolDefinition {
            name: tool.name().to_owned(),
            description: tool.description(&prompt),
            input_schema: tool.input_schema(),
            cache_control: None,
        })
        .collect()
}

/// 与主循环 `wire_tools` 相同的规则：名字排序、内置在前；延迟加载开启时没被引用的延迟工具单独计。`supported` 是
/// 端点支持延迟加载（`models::supports_tool_search`，与主循环同一个判定）。
fn wire(
    tools: &[Arc<dyn Tool>],
    active: &[&Message],
    mode: PermissionMode,
    supported: bool,
) -> Wired {
    let mut sorted: Vec<&Arc<dyn Tool>> = tools.iter().collect();
    sorted.sort_by(|left, right| {
        left.is_mcp()
            .cmp(&right.is_mcp())
            .then_with(|| compare_names(left.name(), right.name()))
    });
    let search = supported
        && sorted.iter().any(|tool| tool.name() == TOOL_SEARCH)
        && sorted.iter().any(|tool| is_deferred(tool.as_ref()));
    let loaded: BTreeSet<String> = if search {
        referenced(active.iter().copied())
    } else {
        BTreeSet::new()
    };
    let waiting = |tool: &Arc<dyn Tool>| {
        search && is_deferred(tool.as_ref()) && !loaded.contains(tool.name())
    };
    let (mcp, builtin): (Vec<_>, Vec<_>) = sorted.into_iter().partition(|tool| tool.is_mcp());
    let listed: Vec<_> = builtin
        .iter()
        .copied()
        .filter(|tool| !waiting(tool) && (search || tool.name() != TOOL_SEARCH))
        .collect();
    let skill: Vec<_> = builtin
        .iter()
        .copied()
        .filter(|tool| tool.name() == SKILL_TOOL)
        .collect();
    Wired {
        listed: definitions(&listed, mode),
        deferred: builtin
            .iter()
            .filter(|tool| waiting(tool))
            .flat_map(|tool| definitions(&[*tool], mode))
            .collect(),
        mcp: definitions(&mcp, mode)
            .into_iter()
            .zip(mcp.iter().map(|tool| !waiting(tool)))
            .collect(),
        skill: definitions(&skill, mode).into_iter().next(),
        search,
    }
}

fn window_source(source: Source) -> WindowSource {
    match source {
        Source::Override => WindowSource::Env,
        Source::ModelDefault => WindowSource::ModelDefault,
        Source::UnknownModel => WindowSource::UnknownModel,
        Source::Auto => WindowSource::Auto,
    }
}

fn skill_source(source: &SkillSource) -> (&'static str, Option<String>) {
    match source {
        SkillSource::Builtin => ("Built-in", None),
        SkillSource::User => ("User", None),
        SkillSource::Project => ("Project", None),
        SkillSource::Managed => ("Managed", None),
        SkillSource::Plugin { plugin } => ("Plugin", Some(plugin.clone())),
    }
}

fn agent_source(source: AgentSource) -> &'static str {
    match source {
        AgentSource::BuiltIn => "Built-in",
        AgentSource::Plugin => "Plugin",
        AgentSource::User => "User",
        AgentSource::Project => "Project",
    }
}

fn failed(error: impl std::fmt::Display) -> KernelError {
    KernelError::Failed {
        message: error.to_string(),
    }
}

impl UsageAnalyzer {
    /// `history` 是会话的完整历史（没有会话时为空）。
    pub(crate) async fn analyze(
        &self,
        query: ContextUsageQuery,
        history: &[Message],
    ) -> Result<ContextUsage, KernelError> {
        let active = history::active(history);
        let active: Vec<&Message> = active.iter().map(AsRef::as_ref).collect();
        // 与主循环同一个能力来源（model.md §11，修复线 F23）：WebSearch 的去留、对话中间的 system。
        let capabilities = self.model.capabilities(&query.selection).await;
        let web_search = capabilities.is_none_or(|found| found.supports_native_web_search);
        let mut tools: Vec<Arc<dyn Tool>> = self
            .tools
            .tools()
            .into_iter()
            .filter(|tool| tool.is_enabled() && (web_search || !tool.needs_native_web_search()))
            .collect();
        let model = query.selection.model.clone();
        // 端点支持延迟加载（工作线 F52：按端点，不按卡片，tools-more.md §3.2）。
        let supported = rust_cli_prompts::models::supports_tool_search(
            &model,
            capabilities.map(|found| found.supports_tool_search),
        );
        // 启用 ToolSearch 时没有 WaitForMcpServers（与主循环的工具快照相同，tools-more.md §3.3 第 4 条，修复线 F36）。
        if supported && tools.iter().any(|tool| tool.name() == TOOL_SEARCH) {
            tools.retain(|tool| tool.name() != WAIT_FOR_MCP_SERVERS);
        }
        let tool_names: Vec<String> = tools.iter().map(|tool| tool.name().to_owned()).collect();
        let per_token = rust_cli_prompts::models::chars_per_token(&model);
        let request = ContextRequest {
            capabilities,
            // 没有会话时规则缓存不参与（analysis 不缓存），会话 ID 只是占位。
            session: query
                .session
                .clone()
                .unwrap_or_else(|| SessionId::new("context-usage")),
            workspace: query.workspace.clone(),
            agent: None,
            source: QuerySource::MainThread,
            mode: query.mode,
            tool_names,
            selection: query.selection.clone(),
            plan_flags: Default::default(),
            mcp: None,
        };
        let analysis = self
            .context
            .analysis(&request, history)
            .await
            .map_err(failed)?;
        let wired = wire(&tools, &active, query.mode, supported);
        let counter = Counter {
            model: &self.model,
            selection: &query.selection,
            cache: &self.cache,
        };
        let custom: Vec<_> = self
            .agents
            .definitions(&query.workspace)
            .await
            .into_iter()
            .filter(|agent| agent.source != AgentSource::BuiltIn)
            .collect();
        let sources: BTreeMap<String, (&'static str, Option<String>)> = self
            .skills
            .skills(&query.workspace)
            .await
            .iter()
            .map(|skill| (skill.name.clone(), skill_source(&skill.source)))
            .collect();
        let instructions: Vec<&str> = analysis
            .mcp_instructions
            .iter()
            .map(|entry| entry.line.as_str())
            .collect();
        let agent_texts: Vec<String> = custom
            .iter()
            .map(|agent| format!("{} {}", agent.agent_type, agent.when_to_use))
            .collect();
        let anchor = rust_cli_compact::measured(&active);
        // 计数并发发出；顺序：a、b、c、d、e、f、g、h、i。
        let mcp_definitions: Vec<ToolDefinition> =
            wired.mcp.iter().map(|(tool, _)| tool.clone()).collect();
        let transcript = anchor.is_none().then(|| {
            let kept = active.iter().copied().filter(|message| {
                !matches!(message, Message::Attachment(found)
                    if found.source == "instructions" || found.source == "mcp_instructions_delta")
            });
            let configured = capabilities.map(|found| found.supports_mid_conversation_system);
            rust_cli_query::api_messages(kept, &model, configured)
        });
        let (mcp_total, sections, memory, listed, deferred, _skill, agents, mcp_text, counted) = futures_util::join!(
            counter.tools(mcp_definitions),
            join_all(
                analysis
                    .sections
                    .iter()
                    .map(|section| counter.text(section))
            ),
            join_all(
                analysis
                    .memory_files
                    .iter()
                    .map(|file| counter.text(&file.content))
            ),
            counter.tools(wired.listed.clone()),
            join_all(
                wired
                    .deferred
                    .iter()
                    .map(|tool| counter.tools(vec![tool.clone()]))
            ),
            async {
                match &wired.skill {
                    Some(skill) => counter.tools(vec![skill.clone()]).await,
                    None => 0,
                }
            },
            join_all(agent_texts.iter().map(|text| counter.text(text))),
            async {
                if instructions.is_empty() {
                    0
                } else {
                    counter.text(&instructions.join("\n\n")).await
                }
            },
            async {
                match transcript {
                    Some(messages) => Some(counter.count(messages, Vec::new(), false).await),
                    None => None,
                }
            },
        );
        // 与自动压缩、footer 同一个来源（compact.md §2，修复线 F61：以前这里读目录，显式覆盖下没有目录，按 200,000）。
        let facts = self.compact.facts(&query.selection);
        let thresholds = &self.compact.thresholds;
        let found = window(&facts, thresholds);
        let measured = Measured {
            model,
            window: found.window,
            source: window_source(found.source),
            auto: self.compact.auto,
            buffer: context_buffer(&facts, thresholds, self.compact.auto),
            sections,
            memory_files: analysis
                .memory_files
                .iter()
                .zip(memory)
                .map(|(file, tokens)| MemoryFileUsage {
                    path: file.path.clone(),
                    kind: file.kind.to_owned(),
                    tokens,
                })
                .collect(),
            listed_tools: listed,
            deferred_tools: deferred
                .into_iter()
                .map(|tokens| tokens.saturating_sub(TOOL_OVERHEAD))
                .collect(),
            mcp_total,
            mcp_tools: wired
                .mcp
                .iter()
                .map(|(tool, loaded)| McpTool {
                    name: tool.name.clone(),
                    estimate: rough(&serde_json::to_string(tool).unwrap_or_default(), 4),
                    loaded: *loaded,
                })
                .collect(),
            tool_search: wired.search,
            mcp_instructions: mcp_text,
            agents: custom
                .iter()
                .zip(agents)
                .map(|(agent, tokens)| AgentUsage {
                    agent_type: agent.agent_type.clone(),
                    source: agent_source(agent.source).to_owned(),
                    tokens,
                })
                .collect(),
            skills: analysis
                .skills
                .iter()
                .map(|entry| {
                    let (source, plugin) = sources
                        .get(&entry.name)
                        .cloned()
                        .unwrap_or(("Built-in", None));
                    SkillUsage {
                        name: entry.name.clone(),
                        source: source.to_owned(),
                        plugin,
                        // 修 bug（F10，context-usage.md D5）：以前一律按 4 字符 / token；现在按模型。
                        tokens: rough(&entry.line, per_token),
                    }
                })
                .collect(),
            messages: match (anchor, counted) {
                (Some((input, tail)), _) => Messages::Measured { input, tail },
                (None, counted) => Messages::Counted(counted.unwrap_or(0)),
            },
        };
        Ok(arith::usage(measured))
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
