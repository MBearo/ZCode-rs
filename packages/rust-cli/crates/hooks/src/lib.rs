//! hooks：事件、matcher 与 command / http / prompt / agent 四类执行（docs/specs/rust-cli/hooks.md）。
//!
//! 输入输出 JSON 见 spec §3、§4；配置是入口层组装的带版本快照（[`HooksConfig`]），
//! 本 crate 不读文件。进程经执行环境端口、网络经 [`HookHttp`] 端口、模型经 [`ModelPort`]、大输出经
//! [`ArtifactStore`]，hook 不直接做 IO。事件专属的处理（反馈文本、attachment 渲染）在这里完成，结果放进
//! [`HookOutcome`]，调用方按字段提交消息、改变流程（spec §6、§7）。
//!
//! | 模块 | 内容 |
//! | --- | --- |
//! | [`config`] | 设置解析与快照 |
//! | `effective` / `plugin` | 来源合并、插件 hooks |
//! | [`node_config`] | Node 配置文件的 `hooks` 段（config.md） |
//! | [`workspace_digest`] | 项目 hooks 的信任 digest（与 Node 逐字节一致） |
//! | `matcher` | match query、matcher、去重与 `if` 条件 |
//! | `input` | 输入 JSON |
//! | `output` / `interpret` | 输出解析、退出码语义 |
//! | `command` / `http` / `prompt` | 执行器 |
//! | `run` | 并发执行 |
//! | `fold` / `fold_session` | 按事件合并成 [`HookOutcome`] |
//! | `persist` | 超过 10000 字符的输出落盘（经 ArtifactStore） |
//! | `texts` | 模型可见的文本 |
//!
//! [`HookHttp`]: rust_cli_kernel_api::hooks::HookHttp
//! [`ModelPort`]: rust_cli_kernel_api::model::ModelPort
//! [`ArtifactStore`]: rust_cli_kernel_api::store::ArtifactStore

mod command;
pub mod config;
mod consts;
mod effective;
mod fold;
mod fold_session;
mod http;
mod input;
mod interpret;
mod listing;
mod matcher;
pub mod node_config;
mod on_error;
mod output;
mod persist;
mod plugin;
mod prompt;
mod result;
mod run;
mod texts;
pub mod workspace_digest;

#[cfg(test)]
mod test_support;

use std::sync::{Arc, PoisonError, RwLock};

use async_trait::async_trait;
use rust_cli_base::units;
use rust_cli_kernel_api::hooks::{
    HookEvent, HookEventName, HookOutcome, HookRunner, StopCondition, StopVerdict,
};
use rust_cli_kernel_api::model::small_fast_model;
use tokio_util::sync::CancellationToken;

pub use config::{HookSource, HooksConfig, HooksSettings};
pub use plugin::PluginHooks;
pub use run::HookPorts;

use crate::effective::Effective;
use crate::fold::Fold;
use crate::http::HttpPolicy;
use crate::run::Batch;

/// 默认超时：UserPromptSubmit 与模型切换 30 秒、SessionEnd 1.5 秒、其他事件 10 分钟。
const PROMPT_TIMEOUT_MS: u64 = 30 * units::MS_PER_SECOND;
const SESSION_END_TIMEOUT_MS: u64 = 1_500;
const DEFAULT_TIMEOUT_MS: u64 = 10 * units::MS_PER_MINUTE;

/// 默认超时（其他事件 10 分钟；UserPromptSubmit 30 秒；SessionEnd 1.5 秒；模型切换 30 秒）。
fn default_timeout_ms(event: HookEventName) -> u64 {
    match event {
        HookEventName::UserPromptSubmit
        | HookEventName::PreModelSwitch
        | HookEventName::PostModelSwitch => PROMPT_TIMEOUT_MS,
        HookEventName::SessionEnd => SESSION_END_TIMEOUT_MS,
        _ => DEFAULT_TIMEOUT_MS,
    }
}

/// 只给出每个 hook 结果、不折叠成消息的事件（“外部”路径）。
fn outside(event: HookEventName) -> bool {
    use HookEventName as E;
    matches!(
        event,
        E::Notification
            | E::SessionEnd
            | E::StopFailure
            | E::PreCompact
            | E::PostCompact
            | E::ConfigChange
            | E::DirectoryAdded
            | E::Elicitation
            | E::ElicitationResult
            | E::CwdChanged
            | E::FileChanged
            | E::InstructionsLoaded
            | E::WorktreeCreate
            | E::WorktreeRemove
    )
}

struct Loaded {
    config: HooksConfig,
    /// 插件的 hooks（与设置快照分开登记，hooks.md §2.2）。
    plugins: Vec<PluginHooks>,
    effective: Effective,
}

/// hook 引擎：持有当前快照，实现 [`HookRunner`]。
pub struct HookEngine {
    ports: HookPorts,
    loaded: RwLock<Arc<Loaded>>,
}

impl HookEngine {
    /// 没有任何 hook 的引擎；配置由入口层经 [`HookEngine::replace`] 交进来。
    pub fn new(ports: HookPorts) -> Self {
        Self::with_config(ports, HooksConfig::default())
    }

    pub fn with_config(ports: HookPorts, config: HooksConfig) -> Self {
        Self {
            ports,
            loaded: RwLock::new(Arc::new(load(config, Vec::new()))),
        }
    }

    /// 换上新的快照；正在运行的事件用旧快照。插件 hooks
    /// 保留。
    pub fn replace(&self, config: HooksConfig) {
        let mut loaded = self.loaded.write().unwrap_or_else(PoisonError::into_inner);
        *loaded = Arc::new(load(config, loaded.plugins.clone()));
    }

    /// 登记插件的 hooks（runtime 在插件加载后调用，hooks.md §2.2）；设置快照保留。读改写在同一把
    /// 写锁里，与 [`HookEngine::replace`] 不会互相冲掉。
    pub fn register_plugins(&self, plugins: Vec<PluginHooks>) {
        let mut loaded = self.loaded.write().unwrap_or_else(PoisonError::into_inner);
        *loaded = Arc::new(load(loaded.config.clone(), plugins));
    }

    fn snapshot(&self) -> Arc<Loaded> {
        self.loaded
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// hook 输入的 `cwd`：调用方给出的，或会话的 shell cwd，再没有时是工作区根。
    async fn cwd(&self, event: &HookEvent) -> String {
        if let Some(cwd) = &event.context.cwd {
            return cwd.as_str().to_owned();
        }
        self.ports
            .shell
            .shell_state(&event.workspace, &event.session)
            .await
            .map_or_else(
                |_| event.workspace.root.as_str().to_owned(),
                |state| state.cwd.as_str().to_owned(),
            )
    }
}

fn load(config: HooksConfig, plugins: Vec<PluginHooks>) -> Loaded {
    let effective = config.effective_with(&plugins);
    Loaded {
        config,
        plugins,
        effective,
    }
}

#[async_trait]
impl HookRunner for HookEngine {
    async fn run(&self, event: HookEvent, cancel: CancellationToken) -> HookOutcome {
        let loaded = self.snapshot();
        if !loaded.config.trusted || cancel.is_cancelled() {
            return HookOutcome::default();
        }
        let Some(matchers) = loaded.effective.matchers.get(&event.name) else {
            return HookOutcome::default();
        };
        let hooks = matcher::select(event.name, matchers, &event.payload);
        if hooks.is_empty() {
            return HookOutcome::default();
        }
        let query = matcher::match_query(event.name, &event.payload).filter(|q| !q.is_empty());
        let hook_name = input::hook_name(event.name.as_str(), query.as_deref());
        let input = input::build(&event, &self.cwd(&event).await).to_string();
        let batch = Batch {
            event: event.name,
            session: &event.session,
            workspace: &event.workspace,
            context: &event.context,
            input,
            default_timeout_ms: default_timeout_ms(event.name),
            http: HttpPolicy {
                allowed_urls: loaded.effective.allowed_urls.as_deref(),
                allowed_env_vars: loaded.effective.allowed_env_vars.as_deref(),
                env: &loaded.config.env,
            },
            prompt_model: loaded.config.prompt_model.as_ref(),
        };
        let mut done = run::run_all(&self.ports, &batch, &hooks, &cancel).await;
        let event_name = event.name.as_str();
        if outside(event.name) {
            // 结果按配置顺序排列（不按完成顺序）。
            done.sort_by_key(|(index, _)| *index);
            let summaries: Vec<_> = done
                .into_iter()
                .map(|(index, executed)| interpret::outside(executed, &hooks[index].display()))
                .collect();
            return fold_session::outside(event.name, &summaries);
        }
        let mut results: Vec<_> = done
            .into_iter()
            .map(|(index, executed)| {
                let hook = &hooks[index];
                let display = hook.display();
                // Node 配置的 `onError: "deny"`（修复线 F68）：PreToolUse 的这个 hook 出错时拦下工具。
                if event.name == HookEventName::PreToolUse && hook.denies_on_error() {
                    let timeout = run::timeout_ms(hook, batch.default_timeout_ms);
                    let result = interpret::repl(executed.clone(), &display, event_name);
                    return on_error::deny(&executed, result, &display, timeout);
                }
                interpret::repl(executed, &display, event_name)
            })
            .collect();
        // 超过 10000 字符的输出先落盘、换成预览，再折叠（hooks.md §4.5，修复线 F15）。
        persist::results(
            self.ports.artifacts.as_ref(),
            &event.session,
            event.name,
            &mut results,
        )
        .await;
        let fold = Fold {
            ids: self.ports.ids.as_ref(),
            event: event_name,
            hook_name: &hook_name,
        };
        use HookEventName as E;
        match event.name {
            E::PreToolUse => fold::pre_tool_use(&fold, &results),
            E::PostToolUse => fold::post_tool_use(&fold, &results),
            E::PostToolUseFailure => fold::post_tool_use_failure(&fold, &results),
            E::PermissionRequest => fold::permission_request(&results),
            E::PermissionDenied => fold::permission_denied(&results),
            E::Stop | E::SubagentStop => fold_session::stop(&fold, &results),
            E::UserPromptSubmit => fold_session::user_prompt_submit(
                &fold,
                &results,
                fold_session::prompt_of(&event.payload),
            ),
            E::SessionStart | E::SubagentStart => fold_session::session_start(&fold, &results),
            _ => fold_session::generic(&fold, &results),
        }
    }

    /// `/goal` 的停止条件（会话级 Stop prompt hook，docs/specs/rust-cli/goal.md §5）：不看配置与信任，
    /// 模型同 prompt hook 的缺省（小模型槽位），超时 30 秒。
    async fn evaluate_stop_condition(
        &self,
        request: StopCondition,
        cancel: CancellationToken,
    ) -> StopVerdict {
        let loaded = self.snapshot();
        let default_model = request
            .event
            .context
            .selection
            .as_ref()
            .map(small_fast_model)
            .or_else(|| loaded.config.prompt_model.clone());
        let input = input::build(&request.event, &self.cwd(&request.event).await).to_string();
        let run = prompt::StopRun {
            model: self.ports.model.as_deref(),
            requester: prompt::Requester {
                session: &request.event.session,
                run: request.event.context.run.as_ref(),
                agent: request.event.context.agent.as_ref(),
            },
            default_model: default_model.as_ref(),
            ids: self.ports.ids.as_ref(),
            condition: &request.condition,
            input: &input,
            transcript: request.transcript,
            timeout_ms: prompt::DEFAULT_TIMEOUT_MS,
        };
        prompt::evaluate(run, cancel).await
    }

    /// 快照里有这个事件的 matcher（不看 match query）；会话据此决定要不要进入等待
    /// （docs/specs/rust-cli/session-lifecycle.md §3.2）。
    fn has_hooks(&self, event: HookEventName) -> bool {
        let loaded = self.snapshot();
        loaded.config.trusted
            && loaded
                .effective
                .matchers
                .get(&event)
                .is_some_and(|matchers| !matchers.is_empty())
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "on_error_tests.rs"]
mod on_error_tests;
