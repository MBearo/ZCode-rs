//! 真实内核的互操作驱动（docs/specs/rust-cli/node-store.md §6 第 11 段），由
//! `scripts/rust-cli/node-interop-runtime.mjs` 调用：用入口同样的部件（LocalKernel、NodeStore、LocalEnvironment、
//! 指向本地模型替身的 Anthropic 端点）打开一个已有会话，发 TUI 里 `/compact`、`/rewind`、`/fork` 发的命令与
//! `--fork-session` 的 `Fork`，等它完成后输出回执 JSON。回合（Submit）由真实的 `zcode -p` 跑，这里只做 `-p` 不支持的会话命令。
//!
//! - `compact <storage-dir> <session> <base-url>`：`Command::Compact`，等这次压缩的 `TurnFinished`；
//! - `rewind <storage-dir> <session> <message> <conversation|code|both>`：`Command::Rewind`（回执等事务结束）；
//! - `branch <storage-dir> <session>`：`Command::Fork { at: None }`（`--fork-session` 的命令；F75 之前是 `/branch`），输出新会话 ID；
//! - `fork <storage-dir> <session> <message>`：`Command::ForkFromInput`（`/fork` 选中一条输入，F75），输出回执；
//! - `options <storage-dir> <session>`：rewind 选择器的条目（每个回合改过的文件与能否恢复代码）。
//!
//! 会话库是 `<storage-dir>/cli/db/db.sqlite`，产物在 `<storage-dir>/cli/artifacts`（与 `node_interop` 相同）。
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use rust_cli_base::secret::RedactedString;
use rust_cli_env_local::LocalEnvironment;
use rust_cli_kernel_api::ids::{CommandId, MessageId, SessionId};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{
    Ack, Command, CommandEnvelope, EndReason, Kernel, KernelEvent, RewindScope, SessionHandle,
    TurnOutcome,
};
use rust_cli_kernel_api::testing::environments;
use rust_cli_model::{AnthropicEndpoint, ProviderModel};
use rust_cli_net::{Egress, NetworkPolicy};
use rust_cli_node_store::{NodeIds, NodeStore, SystemClock};
use rust_cli_paths::{StoragePaths, StorageSettings};
use rust_cli_runtime::{Defaults, LocalKernel, Ports};
use serde_json::{Value, json};

/// 等一个命令完成的上限：本地替身几毫秒就回复，超过说明卡住了，报错退出而不是挂住检查脚本。
const WAIT: Duration = Duration::from_secs(60);

async fn kernel(dir: &str, base_url: Option<&str>) -> Result<LocalKernel> {
    let root = Path::new(dir);
    let db = root.join(rust_cli_paths::DEFAULT_SESSION_DB);
    let paths = StoragePaths::resolve(
        &StorageSettings {
            session_db_path: Some(db.to_str().context("storage dir is not UTF-8")?),
            storage_dir: Some(dir),
        },
        root,
        root,
    );
    let clock = Arc::new(SystemClock::utc());
    let ids = Arc::new(NodeIds::new(clock.clone()));
    let store = Arc::new(NodeStore::new(paths, ids.clone()).with_version("0.1.1"));
    let egress = Arc::new(Egress::new(NetworkPolicy::default()).await?);
    // 压缩的摘要请求发到本地替身；其余命令不调模型，端点照样配好（模型名取会话的选择）。
    let endpoint = AnthropicEndpoint {
        base_url: base_url.unwrap_or("http://127.0.0.1:9").to_owned(),
        api_key: RedactedString::new("sk-interop"),
    };
    let ports = Ports {
        model: Arc::new(ProviderModel::anthropic(egress, endpoint, clock.clone())),
        environments: environments(Arc::new(LocalEnvironment::new(Some(root.to_path_buf())))),
        store: store.clone(),
        artifacts: store,
        clock,
        ids,
    };
    let defaults = Defaults {
        selection: ModelSelection {
            provider: String::new(),
            model: String::new(),
            reasoning: None,
            connection: None,
        },
        mode: PermissionMode::BypassPermissions,
        max_turns: None,
        search_tools: false,
        task_tools: None,
        limits: Default::default(),
    };
    Ok(LocalKernel::new(ports, defaults))
}

fn envelope(id: &str, command: Command) -> CommandEnvelope {
    CommandEnvelope {
        id: CommandId::new(id),
        command,
    }
}

async fn open(kernel: &LocalKernel, session: &str) -> Result<Arc<dyn SessionHandle>> {
    Ok(kernel.open_session(&SessionId::new(session)).await?)
}

fn accepted(ack: Ack) -> Result<Value> {
    match ack {
        Ack::Accepted { result } => Ok(result),
        other => bail!("the command was not accepted: {other:?}"),
    }
}

async fn compact(dir: &str, session: &str, base_url: &str) -> Result<Value> {
    let kernel = kernel(dir, Some(base_url)).await?;
    let handle = open(&kernel, session).await?;
    let after = handle.view().await?.last_seq;
    let mut events = handle.subscribe(Some(after));
    let ack = handle
        .send(envelope(
            "interop-compact",
            Command::Compact { instructions: None },
        ))
        .await;
    accepted(ack)?;
    let outcome = tokio::time::timeout(WAIT, async {
        while let Some(event) = events.next().await {
            if let KernelEvent::TurnFinished { outcome, .. } = event.event {
                return Some(outcome);
            }
        }
        None
    })
    .await
    .context("the compaction did not finish")?;
    let _ = handle.close(EndReason::Other).await;
    match outcome {
        Some(TurnOutcome::Success) => Ok(json!({"compacted": true})),
        other => bail!("the compaction ended with {other:?}"),
    }
}

async fn rewind(dir: &str, session: &str, target: &str, scope: &str) -> Result<Value> {
    let scope = match scope {
        "conversation" => RewindScope::Conversation,
        "code" => RewindScope::Code,
        "both" => RewindScope::Both,
        other => bail!("unknown rewind scope {other}"),
    };
    let kernel = kernel(dir, None).await?;
    let handle = open(&kernel, session).await?;
    let command = Command::Rewind {
        target: MessageId::new(target),
        scope,
    };
    let ack = tokio::time::timeout(WAIT, handle.send(envelope("interop-rewind", command)))
        .await
        .context("the rewind did not finish")?;
    // 拒绝（例如代码回退遇到冲突）也是结果：原文交给脚本比较。
    let result = match ack {
        Ack::Accepted { result } => json!({"accepted": result}),
        Ack::Rejected { reason } => json!({"rejected": reason}),
        Ack::Duplicate => bail!("the rewind was a duplicate"),
    };
    let _ = handle.close(EndReason::Other).await;
    Ok(result)
}

async fn branch(dir: &str, session: &str) -> Result<Value> {
    let kernel = kernel(dir, None).await?;
    let handle = open(&kernel, session).await?;
    let ack = handle
        .send(envelope("interop-branch", Command::Fork { at: None }))
        .await;
    let child = accepted(ack)?;
    let _ = handle.close(EndReason::Other).await;
    Ok(json!({"session": child}))
}

async fn fork(dir: &str, session: &str, input: &str) -> Result<Value> {
    let kernel = kernel(dir, None).await?;
    let handle = open(&kernel, session).await?;
    let command = Command::ForkFromInput {
        input: MessageId::new(input),
    };
    let ack = handle.send(envelope("interop-fork", command)).await;
    let result = accepted(ack)?;
    let _ = handle.close(EndReason::Other).await;
    Ok(result)
}

async fn options(dir: &str, session: &str) -> Result<Value> {
    let kernel = kernel(dir, None).await?;
    let handle = open(&kernel, session).await?;
    let options = handle.rewind_options().await?;
    let _ = handle.close(EndReason::Other).await;
    Ok(serde_json::to_value(options)?)
}

#[expect(clippy::print_stdout, reason = "辅助程序的输出就是 stdout 上的 JSON")]
fn print(value: &Value) -> Result<()> {
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}

async fn run(args: &[String]) -> Result<()> {
    let value = match args {
        [command, dir, session, base_url] if command == "compact" => {
            compact(dir, session, base_url).await?
        }
        [command, dir, session, target, scope] if command == "rewind" => {
            rewind(dir, session, target, scope).await?
        }
        [command, dir, session] if command == "branch" => branch(dir, session).await?,
        [command, dir, session, input] if command == "fork" => fork(dir, session, input).await?,
        [command, dir, session] if command == "options" => options(dir, session).await?,
        _ => bail!(
            "usage: node_kernel compact <dir> <session> <base-url> | rewind <dir> <session> <message> <scope> | branch <dir> <session> | fork <dir> <session> <message> | options <dir> <session>"
        ),
    };
    print(&value)
}

#[expect(
    clippy::disallowed_methods,
    reason = "互操作辅助程序（不是产品代码）读自己的命令行参数"
)]
fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run(&args))
}
