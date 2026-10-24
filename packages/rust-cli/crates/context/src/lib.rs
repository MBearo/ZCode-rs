//! 上下文组装（docs/specs/rust-cli/context.md）。
//!
//! - system prompt 段落（[`system`]）：文本来自 `packages/rust-cli/prompts/`（套产品名
//!   替换表），按固定的顺序与条件拼接；
//! - 每一步的 attachment：环境段（[`environment`]，事实经执行环境端口取）、模型身份、清单
//!   （[`listings`]）、nested memory（[`nested`]）、剩余 token、规则文件（[`instructions`]，加载见
//!   [`rules`] / [`rules_load`]）、会话上下文（[`session_context`]）、日期、署名（[`announce`]）、
//!   计划模式（[`plan_mode`]）、bypass 模式的宣布（[`auto_mode`]）。它们
//!   作为消息落库，resume 后原样重放；发给模型时的位置由 query 的 normalize 决定。

mod analysis;
mod announce;
mod auto_mode;
mod auxiliary;
mod changed;
mod config;
mod directory;
mod environment;
mod input_reminders;
mod instructions;
mod invoked;
mod listings;
mod mcp;
mod nested;
mod plan_mode;
mod post_compact;
mod render;
mod rules;
mod rules_frontmatter;
mod rules_load;
mod rules_markdown;
mod session_context;
mod silent;
mod subagent;
mod system;
mod task_reminder;

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::agents::TaskReader;
use rust_cli_kernel_api::clock::{Clock, Ids};
use rust_cli_kernel_api::env::EnvFacts;
use rust_cli_kernel_api::env_files::PathStyle;
use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::listing::{ListingEntry, ListingSource, Listings};
use rust_cli_kernel_api::local_services::Git;
use rust_cli_kernel_api::machine::{EnvInfo, Files, Shell};
use rust_cli_kernel_api::message::{AttachmentMessage, Message, SystemMessage};
use rust_cli_kernel_api::model::QuerySource;
use rust_cli_kernel_api::orchestration::{
    BuiltContext, ContextBuilder, ContextRequest, OrchestrationError,
};
use rust_cli_kernel_api::tool::ChangedFiles;
use rust_cli_prompts::{models, reminder};
use serde_json::Value;

pub use analysis::{Analysis, MemoryFile};
pub use config::{Attribution, ContextConfig, OutputStyle, Surface};

use crate::instructions::{Lookup, Memo};
use crate::render::since_boundary;
use crate::rules::{RuleFile, Scope};
use crate::system::{AGENT_TOOL, BASH_TOOL, PromptFacts, SKILL_TOOL, system_blocks};

/// 组装上下文需要的端口。上下文都在本机：会话的工作区、规则文件、自动记忆与 git 状态都经本机的端口读
/// （runtime 从注册表的本机取，remote-exec.md §3、§15.8.1）。
#[derive(Clone)]
pub struct ContextPorts {
    /// 环境事实（环境段、规则文件的层级）与路径事实（计划文件）。
    pub info: Arc<dyn EnvInfo>,
    /// 规则文件、自动记忆的 MEMORY.md、计划文件是否存在。
    pub files: Arc<dyn Files>,
    /// 会话的 shell 当前目录（环境段、nested memory）。
    pub shell: Arc<dyn Shell>,
    /// 会话上下文的 git 状态。
    pub git: Arc<dyn Git>,
    pub clock: Arc<dyn Clock>,
    pub ids: Arc<dyn Ids>,
    pub listings: Arc<dyn ListingSource>,
}

/// 还没有接入任何清单来源时用它：延迟工具、agent、skill 都为空，不发对应的 attachment。
pub struct NoListings;

#[async_trait]
impl ListingSource for NoListings {
    async fn listings(&self, _request: &ContextRequest) -> Result<Listings, OrchestrationError> {
        Ok(Listings::default())
    }
}

pub struct PromptAssembler {
    ports: ContextPorts,
    config: ContextConfig,
    /// 规则文件的读取缓存（context.md §11.5）。
    rules: Memo,
    /// nested memory 去重的增量计算（context.md §11.6，修复线 F6）。
    nested: nested::Seen,
    /// 读过的文件在磁盘上被改动（tools-files.md §4.1）：readFileState 的 owner 给出；没有时不查。
    changed: Option<Arc<dyn ChangedFiles>>,
    /// 任务提醒读的任务清单（agents.md §7.4）：agents 建好之后由 runtime 接上一次；没有时不提醒。
    tasks: std::sync::OnceLock<Arc<dyn TaskReader>>,
}

/// 当步 attachment 属于哪个循环（docs/specs/rust-cli/agents.md §4.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Thread<'a> {
    Main,
    /// 子代理，带 agent 类型。
    Subagent(&'a str),
}

impl Thread<'_> {
    /// Explore / Plan 不带规则文件、git 状态与 nested memory。
    fn omits_context(self) -> bool {
        matches!(self, Thread::Subagent(agent_type) if subagent::omits_context(agent_type))
    }
}

/// 最近一次压缩边界的 ID：规则缓存按它失效。
fn last_boundary(history: &[Message]) -> Option<MessageId> {
    history.iter().rev().find_map(|message| match message {
        Message::System(SystemMessage::CompactBoundary(boundary)) => Some(boundary.id.clone()),
        _ => None,
    })
}

impl PromptAssembler {
    /// 会话关闭（session-lifecycle.md §3.3）：释放这个会话的规则文件缓存。
    pub fn session_closed(&self, session: &rust_cli_kernel_api::ids::SessionId) {
        self.rules.forget(session);
        self.nested.forget(session);
    }

    pub fn new(ports: ContextPorts, config: ContextConfig) -> Self {
        Self {
            ports,
            config,
            rules: Memo::default(),
            nested: nested::Seen::default(),
            changed: None,
            tasks: std::sync::OnceLock::new(),
        }
    }

    /// 接上任务提醒读的任务清单（agents 依赖 context，所以建好 agents 之后再接；只接一次）。
    pub fn bind_tasks(&self, tasks: Arc<dyn TaskReader>) {
        if self.tasks.set(tasks).is_err() {
            tracing::error!("the task list reader was bound twice; keeping the first");
        }
    }

    /// 接上“读过的文件被改动”的查询（runtime 给 readFileState 的 owner）。
    #[must_use]
    pub fn with_changed_files(mut self, changed: Arc<dyn ChangedFiles>) -> Self {
        self.changed = Some(changed);
        self
    }

    fn scope<'a>(&'a self, request: &'a ContextRequest, facts: &EnvFacts) -> Scope<'a> {
        Scope {
            files: self.ports.files.as_ref(),
            workspace: &request.workspace,
            style: PathStyle::of_platform(&facts.platform),
            cwd: request.workspace.root.as_str().to_owned(),
            home: facts.home.as_ref().map(|home| home.as_str().to_owned()),
        }
    }

    /// 读一次规则文件（启动时的层级加自动记忆），不经缓存。
    async fn load_rule_files(&self, request: &ContextRequest, facts: &EnvFacts) -> Vec<RuleFile> {
        let scope = self.scope(request, facts);
        let mut loaded = scope.startup(&facts.platform).await;
        if let Some(dir) = &self.config.memory_dir
            && let Some(memory) = scope.auto_memory(dir).await
        {
            loaded.push(memory);
        }
        loaded
    }

    /// 本会话当前边界窗口的规则文件：缓存命中直接用，否则读一次（context.md §11.5）。
    async fn rule_files(
        &self,
        request: &ContextRequest,
        facts: &EnvFacts,
        boundary: Option<MessageId>,
    ) -> (Arc<Vec<RuleFile>>, instructions::Reason) {
        let reason = match self.rules.lookup(&request.session, boundary.as_ref()) {
            Lookup::Hit(files, reason) => return (files, reason),
            Lookup::Miss(reason) => reason,
        };
        let files = Arc::new(self.load_rule_files(request, facts).await);
        let session = request.session.clone();
        self.rules.store(session, boundary, files.clone(), reason);
        (files, reason)
    }

    fn attachment(&self, source: &str, (text, data): (String, Value)) -> AttachmentMessage {
        AttachmentMessage {
            id: self.ports.ids.message(),
            source: source.to_owned(),
            text: reminder::wrap(&text),
            data,
        }
    }

    /// 执行环境的事实（规则文件与会话上下文按项目根）。暂时给不出（例如还没实现、远端断开）时为 None：
    /// 环境段、规则文件与会话上下文都按增量发，下一次拿到事实后会补上。
    async fn facts(&self, request: &ContextRequest) -> Option<EnvFacts> {
        match self.ports.info.facts(&request.workspace).await {
            Ok(facts) => Some(facts),
            Err(error) => {
                tracing::warn!(%error, "environment facts unavailable; skipping the environment, instructions and session context");
                None
            }
        }
    }

    /// 环境段显示的事实：主工作目录是 shell 的当前目录（Bash `cd` 之后跟着变）；规则文件与
    /// 会话上下文仍按项目根（`facts.cwd`）。shell 状态总是查一次（与以前每一步的组装
    /// 相同，执行环境按查询推进后台命令的状态）。
    async fn shown_facts(
        &self,
        request: &ContextRequest,
        facts: Option<&EnvFacts>,
    ) -> Option<EnvFacts> {
        let shell_cwd = self
            .ports
            .shell
            .shell_state(&request.workspace, &request.session)
            .await
            .ok()
            .map(|state| state.cwd);
        facts.map(|facts| EnvFacts {
            cwd: shell_cwd.unwrap_or_else(|| facts.cwd.clone()),
            ..facts.clone()
        })
    }

    /// 当步的 attachment（顺序：环境、模型、清单、nested memory、skill、计划模式、
    /// 静默提醒、剩余 token）。主循环在运行开始与每个工具轮结束时调（query.md §1.1）。`session` 是全部历史
    /// （skill 清单的“已宣布”跨压缩保留，context.md §4），`history` 是最近一次压缩边界之后的部分，`boundary`
    /// 是那条边界。
    ///
    /// 子代理（修复线 F16，agents.md §4.3）在每个工具轮结束时用同一组：子循环与主循环是同一个循环，
    /// 只给主线程的一组与静默提醒不发；环境段按子会话的当前目录（派生时的目录）；剩余 token
    /// 不重新锚定。
    async fn turn_group(
        &self,
        (request, thread): (&ContextRequest, Thread<'_>),
        (session, history): (&[Message], &[Message]),
        boundary: Option<MessageId>,
        listings: &Listings,
        mcp_instructions: &[ListingEntry],
    ) -> Vec<AttachmentMessage> {
        // `/goal` 的说明与 Stop hook 的反馈之后不算 `getAttachments` 那一组（goal.md §8）。
        if !announce::inputs_due(history) {
            return Vec::new();
        }
        let has_tool = |name: &str| request.tool_names.iter().any(|tool| tool == name);
        let mut out = Vec::new();
        // 这一步的输入（context.md §4.4）：只在主线程的输入那一步有。
        let input = (thread == Thread::Main)
            .then(|| input_reminders::turn_input(history))
            .flatten();
        // 输入中间的 skill 提及（工作线 F82）：TUI 里键入的输入、有 Skill 工具时，排在最前面（提及那一组在
        // `environment` 之前，context.md §4.4）。
        if let Some(typed) = input.as_ref().and_then(input_reminders::typed)
            && self.config.surface == Surface::Interactive
            && has_tool(SKILL_TOOL)
        {
            let names = self
                .ports
                .listings
                .skill_mentions(request, typed)
                .await
                .unwrap_or_default();
            for name in names {
                let found = input_reminders::skill_mention(&name);
                out.push(self.attachment(input_reminders::SKILL_MENTION, found));
            }
        }
        let facts = self.facts(request).await;
        // 主工作目录是这个会话的当前目录：子代理的子会话在派生时取父会话当时的目录（F19，tools-files.md §3.3），
        // 与运行开始那一组（`subagent_start`）同一来源。
        let shown = self.shown_facts(request, facts.as_ref()).await;
        if let Some(found) = shown
            .as_ref()
            .and_then(|shown| environment::announce(history, shown))
        {
            out.push(self.attachment(environment::SOURCE, found));
        }
        out.extend(
            announce::model(history, &request.selection)
                .map(|found| self.attachment(announce::MODEL, found)),
        );
        // `ultrathink`（工作线 F82）：生产者顺序在模型身份之后、延迟工具清单之前。
        out.extend(
            input
                .as_ref()
                .and_then(input_reminders::ultrathink)
                .map(|found| self.attachment(input_reminders::ULTRATHINK, found)),
        );
        out.extend(
            listings::deferred_tools(
                history,
                &listings.deferred_tools,
                listings::pending_servers(request).as_deref(),
            )
            .map(|found| self.attachment(listings::DEFERRED_TOOLS, found)),
        );
        if has_tool(AGENT_TOOL) {
            out.extend(
                listings::agents(history, &listings.agents)
                    .map(|found| self.attachment(listings::AGENTS, found)),
            );
        }
        // MCP 服务器的 instructions（ext.md §3.6），在 agent 清单之后。
        out.extend(
            mcp::announce(history, mcp_instructions)
                .map(|found| self.attachment(mcp::SOURCE, found)),
        );
        // 读过的文件在磁盘上被改动（tools-files.md §4.1）：在 nested memory 之前；子代理查自己的记录。
        if let Some(changed) = &self.changed {
            for change in changed
                .changed_files(&request.session, &request.workspace)
                .await
            {
                out.push(self.attachment(changed::SOURCE, changed::render(&change)));
            }
        }
        if let Some(facts) = facts.as_ref().filter(|_| !thread.omits_context()) {
            let (files, _) = self.rule_files(request, facts, boundary).await;
            let scope = self.scope(request, facts);
            let seen = (
                &scope,
                &self.nested,
                &request.session,
                self.ports.shell.as_ref(),
            );
            for found in nested::announce(seen, facts, history, &files).await {
                out.push(self.attachment(nested::SOURCE, found));
            }
        }
        if has_tool(SKILL_TOOL) {
            out.extend(
                listings::skills(session, &listings.skills)
                    .map(|found| self.attachment(listings::SKILLS, found)),
            );
        }
        // 计划模式（工作线 L11c，context.md §13）：生产者顺序紧跟 skill_listing。
        out.extend(self.plan_mode(request, history).await);
        // bypass 模式的宣布（修复线 F43，shell-permissions.md §4.9）：生产者顺序在计划模式之后、静默提醒之前；
        // 子代理同样（不看 agentId）。
        out.extend(
            auto_mode::announce(history, request.mode, &request.tool_names)
                .map(|found| self.attachment(auto_mode::SOURCE, found)),
        );
        // 任务工具很久没用时的提醒（agents.md §7.4）：在 bypass 模式的宣布之后；子代理同样（读顶层会话的清单）。
        if has_tool(task_reminder::REQUIRED_TOOL)
            && let Some(tasks) = self.tasks.get()
            && task_reminder::due(history)
        {
            let listed = tasks.tasks(&request.session).await;
            out.push(self.attachment(task_reminder::SOURCE, task_reminder::render(&listed)));
        }
        // 静默提醒只给主线程。
        let silent_turns = thread == Thread::Main
            && models::card(&request.selection.model).is_some_and(|card| card.silent_turn_reminder);
        if let Some(found) = silent::announce(history, silent_turns) {
            out.push(self.attachment(silent::SOURCE, found));
        }
        if announce::tokens_due(history) {
            let tokens = match thread {
                // 修 bug（F82）：以前只看边界之后的历史，压缩之后剩余 token 回到满额；计数应跨压缩延续（压缩前用掉的
                // 不归还），所以按全部历史从这次输入算起。
                Thread::Main => announce::tokens(session),
                Thread::Subagent(_) => announce::subagent_tokens(history),
            };
            out.push(self.attachment(announce::TOTAL_TOKENS, tokens));
        }
        out
    }

    /// 每一步的上下文宣布，顺序是规则文件、会话上下文、日期，再是署名，在自动压缩之后。`history` 是最近一次
    /// 压缩边界之后的部分。
    ///
    /// 子代理同样每一步宣布（子循环与主循环是同一个循环）；Explore / Plan 不宣布规则文件与会话上下文（F16 之前在
    /// `subagent.rs` 里另写一份，现在共用）。
    async fn announcements(
        &self,
        (request, thread): (&ContextRequest, Thread<'_>),
        history: &[Message],
        boundary: Option<MessageId>,
    ) -> Vec<AttachmentMessage> {
        let has_tool = |name: &str| request.tool_names.iter().any(|tool| tool == name);
        let mut out = Vec::new();
        let facts = self.facts(request).await;
        if let Some(facts) = facts.as_ref().filter(|_| !thread.omits_context()) {
            let (files, reason) = self.rule_files(request, facts, boundary).await;
            out.extend(
                instructions::announce(history, &files, reason)
                    .map(|found| self.attachment(instructions::SOURCE, found)),
            );
            // `--system-prompt` 时 systemContext 为空：不发 git 状态（cli-args.md §4.2）。
            if self.config.custom_system_prompt.is_none() {
                let git = self.ports.git.as_ref();
                out.extend(
                    session_context::announce(git, &request.workspace, facts, history)
                        .await
                        .map(|found| self.attachment(session_context::SOURCE, found)),
                );
            }
        }
        out.extend(
            announce::date(history, &self.ports.clock.today())
                .map(|found| self.attachment(announce::DATE, found)),
        );
        out.extend(
            announce::attribution(history, &self.config.attribution, has_tool(BASH_TOOL))
                .map(|found| self.attachment(announce::ATTRIBUTION, found)),
        );
        out
    }
}

#[async_trait]
impl ContextBuilder for PromptAssembler {
    async fn build(
        &self,
        request: ContextRequest,
        history: &[Message],
    ) -> Result<BuiltContext, OrchestrationError> {
        // 带历史的清单：条件 skill 的激活从历史推导（ext.md §2.6）。
        let listings = self
            .ports
            .listings
            .listings_for(&request, since_boundary(history))
            .await?;
        let facts = PromptFacts {
            surface: self.config.surface,
            system_turns: crate::system::facts_system_turns(&request),
            tool_names: &request.tool_names,
            has_skills: !listings.skills.is_empty(),
            memory_dir: self.config.memory_dir.as_deref(),
            output_style: self.config.output_style.as_ref(),
            custom: self.config.custom_system_prompt.as_deref(),
            append: self.config.append_system_prompt.as_deref(),
        };
        let system = system_blocks(&facts);
        // 当步的 attachment 在 `start_attachments` / `turn_attachments`（query.md §1.1；子代理同样，F16）；摘要请求见
        // `auxiliary.rs`（context.md §4.1）。
        let attachments = match &request.source {
            QuerySource::MainThread => {
                let boundary = last_boundary(history);
                let window = since_boundary(history);
                self.announcements((&request, Thread::Main), window, boundary)
                    .await
            }
            QuerySource::Subagent(agent_type) => {
                let boundary = last_boundary(history);
                let window = since_boundary(history);
                let thread = Thread::Subagent(agent_type);
                self.announcements((&request, thread), window, boundary)
                    .await
            }
            QuerySource::Compact => self.auxiliary_announcements(&request, history).await,
            QuerySource::Side(_) => Vec::new(),
        };
        Ok(BuiltContext {
            system,
            user_context: Vec::new(),
            attachments,
        })
    }

    /// 当步的 attachment（query.md §1.1）：主线程与子代理的工具轮结束时（子代理见 agents.md
    /// §4.3）；摘要与旁路请求没有。
    async fn turn_attachments(
        &self,
        request: ContextRequest,
        history: &[Message],
    ) -> Result<Vec<AttachmentMessage>, OrchestrationError> {
        let thread = match &request.source {
            QuerySource::MainThread => Thread::Main,
            QuerySource::Subagent(agent_type) => Thread::Subagent(agent_type),
            QuerySource::Compact | QuerySource::Side(_) => return Ok(Vec::new()),
        };
        let window = since_boundary(history);
        let listings = self.ports.listings.listings_for(&request, window).await?;
        let mcp = self.ports.listings.mcp_instructions(&request).await?;
        let boundary = last_boundary(history);
        let windows = (history, window);
        Ok(self
            .turn_group((&request, thread), windows, boundary, &listings, &mcp)
            .await)
    }

    /// 一次运行开始时的当步 attachment：主线程同 `turn_attachments`（人类输入之后）；子代理是进入
    /// 子循环前的那一组（修复线 F16，agents.md §4.3）。
    ///
    /// 修 bug（F16）：以前子代理在每一步的 `build` 里给出全部 attachment，工具轮之后没有 agent
    /// 清单与剩余 token，计划模式提醒提前到第一个请求。
    async fn start_attachments(
        &self,
        request: ContextRequest,
        history: &[Message],
    ) -> Result<Vec<AttachmentMessage>, OrchestrationError> {
        if !matches!(request.source, QuerySource::Subagent(_)) {
            return self.turn_attachments(request, history).await;
        }
        let window = since_boundary(history);
        let listings = self.ports.listings.listings_for(&request, window).await?;
        Ok(self
            .subagent_start(&request, (history, window), &listings)
            .await)
    }
}

/// 测试用：主循环运行开始那一步看到的全部 attachment，按落库顺序（运行开始的当步 attachment 在前，再是 `build`
/// 的上下文宣布，中间没有 UserPromptSubmit 的结果；query.md §1.1）。
#[cfg(test)]
impl PromptAssembler {
    pub(crate) async fn step(
        &self,
        request: ContextRequest,
        history: &[Message],
    ) -> Result<BuiltContext, OrchestrationError> {
        let mut attachments = self.start_attachments(request.clone(), history).await?;
        let mut seen: Vec<Message> = history.to_vec();
        seen.extend(attachments.iter().cloned().map(Message::Attachment));
        let mut built = self.build(request, &seen).await?;
        attachments.append(&mut built.attachments);
        built.attachments = attachments;
        Ok(built)
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "rules_flow_tests.rs"]
mod rules_flow_tests;

#[cfg(test)]
#[path = "automem_tests.rs"]
mod automem_tests;

#[cfg(test)]
#[path = "auxiliary_tests.rs"]
mod auxiliary_tests;

#[cfg(test)]
#[path = "post_compact_tests.rs"]
mod post_compact_tests;
