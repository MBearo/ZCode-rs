//! 上下文用量（`/context`，docs/specs/rust-cli/context-usage.md）：从会话视图、主循环的工具注册表、上下文
//! 分析与压缩的窗口规则现算，各部分是本地估算（`counts`，工作线 F93 起不发 count_tokens），算术在 `arith`。只读，不改会话。

mod arith;
mod counts;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

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
use rust_cli_kernel_api::tool_search::{TOOL_SEARCH, compare_names, is_deferred, loaded};
use rust_cli_prompts::models::{self, ToolSearchSetting};

use self::arith::{McpTool, Measured, Messages};
use self::counts::{Counter, TOOL_OVERHEAD, rough};

/// 分析要用的能力（runtime 组装时给出，与主循环共用同一批实例）。
pub(crate) struct UsageAnalyzer {
    pub(crate) model: Arc<dyn ModelPort>,
    pub(crate) context: Arc<PromptAssembler>,
    pub(crate) tools: Arc<dyn ToolRegistry>,
    pub(crate) agents: Arc<AgentCatalog>,
    pub(crate) skills: Arc<SkillCatalog>,
    pub(crate) compact: CompactConfig,
    /// `ENABLE_TOOL_SEARCH`（工作线 F89，tools-more.md §3.2.1）：自动时这一步开不开与主循环同一个判定。
    pub(crate) tool_search: ToolSearchSetting,
}

/// 主循环发给模型的工具，按 `/context` 的估算分组（§3：d、e、a）。
#[derive(Default)]
struct Wired {
    listed: Vec<ToolDefinition>,
    deferred: Vec<ToolDefinition>,
    mcp: Vec<(ToolDefinition, bool)>,
    search: bool,
}

/// 一组工具的定义：描述按“这一组”作为可见工具生成——估算的子集当作 `tools` 传给每个
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

/// 与主循环 `wire_tools` 相同的规则：名字排序、内置在前；延迟加载开启时没被引用的延迟工具单独计。`supported` 是这一步
/// ToolSearch 开着（`rust_cli_query::step_tool_search` 不是关，与主循环同一个判定；原生与本地的计数相同，本地只是不发占位工具
/// 与标记）。
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
    let carried: BTreeSet<String> = if search {
        // 同主循环：压缩边界带下来的也算已加载（compact.md §3.5）。
        loaded(active.iter().copied())
    } else {
        BTreeSet::new()
    };
    let waiting = |tool: &Arc<dyn Tool>| {
        search && is_deferred(tool.as_ref()) && !carried.contains(tool.name())
    };
    let (mcp, builtin): (Vec<_>, Vec<_>) = sorted.into_iter().partition(|tool| tool.is_mcp());
    let listed: Vec<_> = builtin
        .iter()
        .copied()
        .filter(|tool| !waiting(tool) && (search || tool.name() != TOOL_SEARCH))
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
        // 有 ToolSearch 时没有 WaitForMcpServers（与主循环的工具快照相同，tools-more.md §3.3 第 4 条，修复线 F36）。
        if tools.iter().any(|tool| tool.name() == TOOL_SEARCH) {
            tools.retain(|tool| tool.name() != WAIT_FOR_MCP_SERVERS);
        }
        let tool_names: Vec<String> = tools.iter().map(|tool| tool.name().to_owned()).collect();
        // 这一步的 ToolSearch 形式：与主循环同一个判定（工作线 F89：`ENABLE_TOOL_SEARCH` 与自动的门槛，不看模型；F84 时
        // 禁用表里的模型不开，tools-more.md §3.2.1）。
        let prompt = PromptContext {
            tool_names: tool_names.clone(),
            mode: query.mode,
        };
        let step = rust_cli_query::step_tool_search(
            &tools,
            &prompt,
            (self.tool_search, &model, capabilities.as_ref()),
        );
        let per_token = models::chars_per_token(&model);
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
        let wired = wire(&tools, &active, query.mode, step.is_on());
        let counter = Counter { per_token };
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
        let anchor = rust_cli_compact::measured(&active);
        // 各部分的本地估算，顺序：a、b、c、d、e、g、h、i（context-usage.md §3）。修改（工作线 F93，用户决定 2026-10-24）：
        // 以前并发发 count_tokens（另有只用于对齐基准统计的 Skill 一项 f），失败时才估算；现在不发模型请求。
        let mcp_definitions: Vec<ToolDefinition> =
            wired.mcp.iter().map(|(tool, _)| tool.clone()).collect();
        let mcp_total = counter.tools(&mcp_definitions);
        let sections: Vec<u64> = analysis
            .sections
            .iter()
            .map(|section| counter.text(section))
            .collect();
        let memory: Vec<u64> = analysis
            .memory_files
            .iter()
            .map(|file| counter.text(&file.content))
            .collect();
        let listed = counter.tools(&wired.listed);
        let agents: Vec<u64> = custom
            .iter()
            .map(|agent| counter.text(&format!("{} {}", agent.agent_type, agent.when_to_use)))
            .collect();
        let mcp_text = if instructions.is_empty() {
            0
        } else {
            counter.text(&instructions.join("\n\n"))
        };
        // 消息：没有用量锚点时估算规范化后的活动历史（F89 到 F93 之间先经 ToolSearch 的降级函数，那只为计数请求服务）。
        let counted = anchor.is_none().then(|| {
            let kept = active.iter().copied().filter(|message| {
                !matches!(message, Message::Attachment(found)
                    if found.source == "instructions" || found.source == "mcp_instructions_delta")
            });
            let configured = capabilities.map(|found| found.supports_mid_conversation_system);
            counter.count(&rust_cli_query::api_messages(kept, &model, configured), &[])
        });
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
            deferred_tools: wired
                .deferred
                .iter()
                .map(|tool| {
                    counter
                        .tools(std::slice::from_ref(tool))
                        .saturating_sub(TOOL_OVERHEAD)
                })
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
