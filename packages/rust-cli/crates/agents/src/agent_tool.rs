//! Agent 工具（docs/specs/rust-cli/agents.md §2）：描述与 schema 在 `prompts/tools/Agent.json`；
//! 前台等子代理结束，后台立即返回“已启动”（缺省为后台）。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::message::ToolResultContent;
use rust_cli_kernel_api::permission::PermissionDecision;
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext, Validation,
};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::Core;
use crate::child::Parent;
use crate::definitions::{AgentDefinition, GENERAL_PURPOSE};
use crate::live::{AgentState, LiveAgent};
use crate::report::{self, Ending};
use crate::routing;
use crate::spawn;

pub(crate) const NAME: &str = "Agent";

#[derive(Debug, Deserialize)]
struct Input {
    description: String,
    prompt: String,
    #[serde(default)]
    subagent_type: Option<String>,
    #[serde(default)]
    effort: Option<String>,
    #[serde(default)]
    run_in_background: Option<bool>,
}

pub(crate) fn text_output(text: String, is_error: bool) -> ToolOutput {
    ToolOutput {
        content: vec![ToolResultContent::Text { text }],
        is_error,
        display: Value::Null,
    }
}

/// 只给界面的结局（结果的 `status` 与统计，docs/specs/rust-cli/tui-display.md §2.1）：one-shot agent
/// 的结果没有 `<usage>` 尾注，界面的 `Done (…)` 行靠它。
/// 另带子会话与类型（kernel-api `agents::subagent_link`）：会话 actor 把调用关联到子会话落库，Node 的子代理查询
/// 靠它找到一次性子代理（修复线 F2）。
fn with_display(mut output: ToolOutput, entry: &LiveAgent, mut display: Value) -> ToolOutput {
    display["agentId"] = entry.agent.as_str().into();
    display["childSessionId"] = entry.child.as_str().into();
    display["agentType"] = entry.definition.agent_type.as_str().into();
    output.display = serde_json::json!({ (rust_cli_kernel_api::agents::DISPLAY_KEY): display });
    output
}

fn launched_output(entry: &LiveAgent) -> ToolOutput {
    with_display(
        text_output(report::launched(entry.agent.as_str()), false),
        entry,
        serde_json::json!({ "status": "async_launched" }),
    )
}

fn parse(input: &Value) -> Result<Input, String> {
    serde_json::from_value(input.clone()).map_err(|error| error.to_string())
}

pub struct AgentTool {
    pub(crate) core: Arc<Core>,
}

impl AgentTool {
    async fn definition(
        &self,
        input: &Input,
        context: &ToolUseContext,
    ) -> Result<AgentDefinition, String> {
        let wanted = input.subagent_type.as_deref().unwrap_or(GENERAL_PURPOSE);
        // fork 不在 agent 清单里，只在 fork 打开时认（修复线 L-A，agents.md §8.1.1）。
        if self.core.fork && wanted == crate::fork::FORK {
            return Ok(crate::fork::definition());
        }
        let definitions = self.core.catalog.definitions(&context.workspace).await;
        if let Some(found) = definitions.iter().find(|agent| agent.agent_type == wanted) {
            return Ok(found.clone());
        }
        let available: Vec<&str> = definitions
            .iter()
            .map(|agent| agent.agent_type.as_str())
            .collect();
        Err(format!(
            "Agent type '{wanted}' not found. Available agents: {}",
            available.join(", ")
        ))
    }
}

#[async_trait]
impl Tool for AgentTool {
    fn name(&self) -> &str {
        NAME
    }

    fn aliases(&self) -> &[&str] {
        // 仍接受旧名 `Task`。
        &["Task"]
    }

    fn input_schema(&self) -> Value {
        // fork 打开时用对照实现的 fork 写法：没有 `run_in_background`（修复线 L-A，agents.md §8.1.1）。
        if self.core.fork
            && let Some(fork) = rust_cli_prompts::agents::fork_tool()
        {
            return fork.input_schema.clone();
        }
        rust_cli_prompts::tool(NAME).map_or(Value::Null, |prompt| prompt.input_schema.clone())
    }

    /// Agent 的 schema 是宽松对象（agents.md §2，修复线 F51）：schema 外的键（例如删掉的 `model`，F51b 删掉的
    /// `isolation`）丢掉、不报错。以前 `validate_input` 拒绝 isolation，现在管线已丢掉这个键，那段检查删掉。
    fn strips_unknown_keys(&self) -> bool {
        true
    }

    fn description(&self, _context: &PromptContext) -> String {
        if self.core.fork
            && let Some(fork) = rust_cli_prompts::agents::fork_tool()
        {
            return fork.description.clone();
        }
        rust_cli_prompts::tool(NAME).map_or_else(String::new, |prompt| prompt.description.clone())
    }

    /// 权限交给子代理自己的工具，Agent 本身算只读。
    fn is_read_only(&self, _input: &Value) -> bool {
        true
    }

    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        true
    }

    fn max_result_size_chars(&self) -> Option<usize> {
        Some(100_000)
    }

    async fn validate_input(&self, input: &Value, context: &ToolUseContext) -> Validation {
        let invalid = |message: String| Validation::Invalid {
            message,
            error_code: None,
        };
        let input = match parse(input) {
            Ok(input) => input,
            Err(message) => return invalid(message),
        };
        match self.definition(&input, context).await {
            Ok(_) => Validation::Ok,
            Err(message) => invalid(message),
        }
    }

    async fn check_permissions(
        &self,
        _input: &Value,
        _context: &ToolUseContext,
    ) -> PermissionDecision {
        // 除 auto 模式的分类器外，派生子代理总是允许（子代理自己的工具仍走权限）。
        PermissionDecision::Allow {
            updated_input: None,
        }
    }

    async fn call(
        &self,
        input: Value,
        context: ToolUseContext,
        progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let raw = input.clone();
        let input = parse(&input).map_err(|message| ToolError::InvalidInput { message })?;
        let definition = self
            .definition(&input, &context)
            .await
            .map_err(|message| ToolError::InvalidInput { message })?;
        let core = &self.core;
        let Some(parent) = core.runs.get(&context.run) else {
            return Err(ToolError::Failed {
                message: "the parent run is not registered".to_owned(),
            });
        };
        // 子代理里派生时同样按参数（以前总在前台：子会话没有 actor 接收通知）。嵌套的后台子代理的
        // 通知送给派生它的子代理（owner），由 `routing` 投递（agents.md §5.1）。fork 打开时一律在后台（对照实现
        // 同样，修复线 L-A）。
        let background =
            self.core.fork || input.run_in_background.unwrap_or(true) || definition.background;
        let fork = definition.agent_type == crate::fork::FORK && self.core.fork;
        // fork 继承父会话已提交的对话（agents.md §8.1.2）；会话 actor 是它的 owner。
        let inherited = if fork {
            match context.interactions.conversation(&context.session).await {
                Some(history) => Some(crate::fork::prefix(
                    core.ports.ids.as_ref(),
                    history,
                    &context.tool_use,
                    raw,
                    &parent.selection.model,
                )),
                None => {
                    return Err(ToolError::Failed {
                        message: "fork is only available in the main conversation".to_owned(),
                    });
                }
            }
        } else {
            None
        };
        let owner = context.agent.clone();
        let depth = owner
            .as_ref()
            .and_then(|owner| core.live.get(owner))
            .map_or(1, |owner| owner.depth + 1);
        let (agent, child) = spawn::ids(core);
        let entry = LiveAgent {
            agent,
            child,
            parent: context.session.clone(),
            root: core.live.root_of(&context.session),
            owner,
            depth,
            tool_use: Some(context.tool_use.clone()),
            description: input.description.clone(),
            // fork 总用父运行的模型与档位（`effort` 参数忽略）。
            selection: if fork {
                parent.selection.clone()
            } else {
                spawn::selection(
                    &parent.selection,
                    definition.model.as_deref(),
                    input.effort.clone().or_else(|| definition.effort.clone()),
                )
            },
            mode: spawn::mode(parent.mode, definition.permission_mode),
            definition: Arc::new(definition),
            workspace: context.workspace.clone(),
            // 执行管线已按 Agent 的 `machine` 换好了机器（R3）：远端时记下它，子代理的工具缺省在那里跑。
            machine: (!context.machine.is_local()).then(|| context.machine.name().to_owned()),
            background,
            // 前台的条目也记：停放后转成后台、之后的续跑都按后台运行（agents.md §4.1.1）。
            asks: core
                .background_prompts
                .then(|| context.interactions.lasting(&context.session))
                .flatten()
                .map(crate::child::Asks),
            state: AgentState::Finished(Ending::Completed),
            shown: crate::live::Shown::default(),
            fork_prefix: inherited.map(Arc::new),
        };
        let first = if fork {
            rust_cli_prompts::agents::fork_directive(&input.prompt)
        } else {
            input.prompt.clone()
        };
        let (entry, opening) =
            spawn::create(core, entry, &parent, &first)
                .await
                .map_err(|error| ToolError::Failed {
                    message: error.to_string(),
                })?;
        // 子会话里后台命令的通知送给这个子代理（agents.md §5.2）。
        crate::shell_notices::watch(core, &entry);
        if background {
            let output = launched_output(&entry);
            spawn::background(core, entry, opening);
            return Ok(output);
        }
        let parent = Parent {
            session: context.session.clone(),
            interactions: context.interactions.clone(),
            progress,
        };
        let finished = spawn::drive(
            core,
            &entry,
            opening,
            Some(parent),
            Some(context.cancel.clone()),
        )
        .await;
        let settled = routing::settle(core, &entry.agent, &finished);
        routing::wake(core, settled.wakes).await;
        if settled.parked {
            // 还有在跑的后台子代理（交互模式）：转成后台、停放，父调用回“已启动”（`async_launched`）。
            return Ok(launched_output(&entry));
        }
        match (finished.ending, finished.error) {
            (Ending::Killed, _) => Err(ToolError::Cancelled),
            (_, Some(error)) => Err(ToolError::Failed {
                message: error.to_string(),
            }),
            (_, None) => {
                let trailer = !entry.definition.one_shot();
                let text = report::handback(entry.agent.as_str(), &finished.report, trailer);
                let report = &finished.report;
                Ok(with_display(
                    text_output(text, false),
                    &entry,
                    serde_json::json!({
                        "status": "completed",
                        "toolUses": report.tool_uses,
                        "tokens": report.tokens,
                        "durationMs": report.duration_ms,
                    }),
                ))
            }
        }
    }
}
