//! 并发执行一次事件匹配到的 hook（docs/specs/rust-cli/hooks.md §4.3）。
//!
//! 按完成顺序收集（`FuturesUnordered`），不另起任务。

use std::sync::Arc;

use futures_util::stream::{FuturesUnordered, StreamExt};
use rust_cli_kernel_api::clock::Ids;
use rust_cli_kernel_api::env::{Environment, OperationContext, Origin, WorkspaceRef};
use rust_cli_kernel_api::hooks::{HookContext, HookEventName, HookHttp};
use rust_cli_kernel_api::ids::{Generation, OwnerEpoch, SessionId};
use rust_cli_kernel_api::model::{ModelPort, ModelSelection, small_fast_model};
use rust_cli_kernel_api::store::ArtifactStore;
use tokio_util::sync::CancellationToken;

use crate::command::{self, CommandRun};
use crate::config::HookCommand;
use crate::http::{self, HttpPolicy};
use crate::prompt::{self, PromptRun};
use crate::result::{Executed, HookResult};

/// 执行 hook 需要的端口。
#[derive(Clone)]
pub struct HookPorts {
    pub env: Arc<dyn Environment>,
    pub ids: Arc<dyn Ids>,
    /// prompt hook 的模型；没有时 prompt hook 报非阻塞错误。
    pub model: Option<Arc<dyn ModelPort>>,
    /// http hook 的出口；没有时 http hook 报非阻塞错误。
    pub http: Option<Arc<dyn HookHttp>>,
    /// 执行环境的当前实例（命令 hook 的操作上下文）。
    pub generation: Generation,
    /// 超过 10000 字符的 hook 输出落在这里（与工具结果同一个存储，hooks.md §4.5）。
    pub artifacts: Arc<dyn ArtifactStore>,
}

/// 一次事件的执行参数。
pub(crate) struct Batch<'a> {
    pub event: HookEventName,
    pub session: &'a SessionId,
    pub workspace: &'a WorkspaceRef,
    pub context: &'a HookContext,
    pub input: String,
    pub default_timeout_ms: u64,
    pub http: HttpPolicy<'a>,
    pub prompt_model: Option<&'a ModelSelection>,
}

pub(crate) fn timeout_ms(hook: &HookCommand, default: u64) -> u64 {
    match hook.timeout() {
        // 设置里是秒。
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "timeout 已校验为正数"
        )]
        Some(seconds) => (seconds * 1000.0) as u64,
        None => default,
    }
}

fn operation(ports: &HookPorts, batch: &Batch<'_>) -> OperationContext {
    OperationContext {
        environment: batch.workspace.environment.clone(),
        generation: ports.generation.clone(),
        owner_epoch: OwnerEpoch::LOCAL,
        operation: ports.ids.operation(),
        origin: match &batch.context.run {
            Some(run) => Origin::Agent {
                session: batch.session.clone(),
                run: run.clone(),
            },
            None => Origin::User {
                session: Some(batch.session.clone()),
            },
        },
    }
}

async fn execute(
    ports: &HookPorts,
    batch: &Batch<'_>,
    hook: &HookCommand,
    cancel: CancellationToken,
) -> Executed {
    let timeout = timeout_ms(hook, batch.default_timeout_ms);
    match hook {
        HookCommand::Command(command) => {
            let run = CommandRun {
                env: ports.env.as_ref(),
                workspace: batch.workspace,
                operation: operation(ports, batch),
                hook: command,
                input: &batch.input,
                timeout_ms: timeout,
            };
            Executed::Raw(command::run(run, cancel).await)
        }
        HookCommand::Http(hook) => Executed::Raw(
            http::run(
                ports.http.as_deref(),
                hook,
                &batch.input,
                timeout,
                &batch.http,
                cancel,
            )
            .await,
        ),
        HookCommand::Prompt(hook) => {
            // `model` 缺省时：缺省模型经小模型槽位从会话当前的主模型得出；调用方没给时用配置的
            // 回退值（model.md §6.1、hooks.md §5.3）。
            let default_model = batch
                .context
                .selection
                .as_ref()
                .map(small_fast_model)
                .or_else(|| batch.prompt_model.cloned());
            let run = PromptRun {
                model: ports.model.as_deref(),
                requester: prompt::Requester {
                    session: batch.session,
                    run: batch.context.run.as_ref(),
                    agent: batch.context.agent.as_ref(),
                },
                default_model: default_model.as_ref(),
                hook,
                input: &batch.input,
                // prompt hook 的默认超时是 30 秒。
                timeout_ms: hook.timeout.map_or(prompt::DEFAULT_TIMEOUT_MS, |_| timeout),
                stop_event: matches!(
                    batch.event,
                    HookEventName::Stop | HookEventName::SubagentStop
                ),
            };
            Executed::Result(Box::new(prompt::run(run, cancel).await))
        }
        HookCommand::Agent(_) => Executed::Result(Box::new(HookResult::non_blocking(
            "Agent hooks are not supported yet",
        ))),
    }
}

/// 并发执行，按完成顺序返回 `(在 hooks 里的位置, 结果)`。
pub(crate) async fn run_all(
    ports: &HookPorts,
    batch: &Batch<'_>,
    hooks: &[HookCommand],
    cancel: &CancellationToken,
) -> Vec<(usize, Executed)> {
    let mut running: FuturesUnordered<_> = hooks
        .iter()
        .enumerate()
        .map(|(index, hook)| {
            let cancel = cancel.clone();
            async move { (index, execute(ports, batch, hook, cancel).await) }
        })
        .collect();
    let mut done = Vec::with_capacity(hooks.len());
    while let Some(result) = running.next().await {
        done.push(result);
    }
    done
}
