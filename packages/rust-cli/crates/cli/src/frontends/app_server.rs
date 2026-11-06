//! `zcode app-server --stdio [--surface desktop]`（docs/specs/rust-cli/app-protocol.md §3.7、§10）：App 的 Host 经 stdio
//! 拉起的协议前端。参数照 Host 现在给 Node 的（`ZA:zcodeAgentProcessManager.ts` 的 `applyPresentationSurfaceToCommand`）。
//!
//! 组装与 `-p`、交互界面共用（[`crate::assemble`]，规格是 `Controller::app`：交互形态的提示词与工具；不问工作区信任、不生成
//! 标题、不连远端机器、不登记跨会话通信、只接受本地主 env、root 下的 bypass 照 Node 不拒绝）；区别只在注入的东西：平台层的
//! 账号凭据来源经 Host（G8），前端是 `protocol-zcode`。写者先起；读完配置、库的位置定了之后报 `startup/storageState` 的
//! `checking`（Host 要求拉起后 30 s 内有首帧），接着查会话库（打不开时报 `failed`），建好内核、开始连接 MCP 之后报 `ready`。

use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use rust_cli_base::TaskScope;
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::store::{SessionFilter, SessionStore};
use rust_cli_node_store::LOCAL_ENVIRONMENT;
use rust_cli_protocol_core::connection::{self, Options};
use rust_cli_protocol_zcode::{Context, Defaults, HostHeaders, Startup};

use crate::assemble::{self, Controller};
use crate::process::{ControllerRun, Start};

/// 退出时等进程级任务收尾的上限（存活的上限，不用来同步状态）。
const SCOPE_CLOSE_GRACE: Duration = Duration::from_secs(2);

/// 子命令的参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Parsed {
    /// `--surface`（Host 给 `desktop`）。P1 不按它改提示词（app-protocol.md §13 第 4 项）。
    pub(crate) surface: Option<String>,
}
/// `app-server` 子命令；不是它时为 None。只认 `--stdio` 与 `--surface`。
pub(crate) fn parse(args: &[String]) -> Option<Result<Parsed, String>> {
    let (first, rest) = args.split_first()?;
    if first != "app-server" {
        return None;
    }
    let mut stdio = false;
    let mut surface = None;
    let mut rest = rest.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--stdio" => stdio = true,
            "--surface" => match rest.next() {
                Some(value) => surface = Some(value.clone()),
                None => {
                    return Some(Err(
                        "error: option '--surface <surface>' argument missing".into()
                    ));
                }
            },
            other => match other.strip_prefix("--surface=") {
                Some(value) => surface = Some(value.to_owned()),
                None => return Some(Err(format!("error: unknown option '{other}'"))),
            },
        }
    }
    if !stdio {
        return Some(Err("error: app-server needs --stdio".to_owned()));
    }
    Some(Ok(Parsed { surface }))
}

/// 会话库的不透明 ID：库路径的散列（固定的键，同一个库总是同一个 ID；不带路径）。
fn database_id(path: &std::path::Path) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    format!("session-db-{:016x}", hasher.finish())
}

pub(crate) async fn run(parsed: Parsed) -> Result<u8> {
    // 日志只写文件：stdout 是协议（logging.md §6）。
    let run = ControllerRun::begin(Start {
        mode: "app-server",
        surface: Some(parsed.surface.as_deref().unwrap_or("none")),
    })?;
    let scope = TaskScope::root("app-server", |panic| {
        tracing::error!(event = "task.panic", scope = %panic.scope, task = panic.task, message = %panic.message, "a task panicked");
    });
    let (peer, writer) =
        connection::start(tokio::io::stdout(), &scope.spawner(), Options::default());
    let context = Arc::new(Context::new(peer, run.clock.clone()));
    let result = serve(&run, context, scope.spawner()).await;
    // 写者写完队列里剩下的帧再退出。
    drop(writer);
    let _ = scope.close(SCOPE_CLOSE_GRACE).await;
    run.finish(result.map(|()| 0)).await
}

async fn serve(
    run: &ControllerRun,
    context: Arc<Context>,
    spawner: rust_cli_base::Spawner,
) -> Result<()> {
    let ControllerRun {
        facts,
        paths,
        clock,
        stop,
        model_io,
        ..
    } = run;
    let spec = Controller::app(Arc::new(HostHeaders::new(context.clone())));
    let opened = assemble::open(&spec, facts, (paths, clock)).await?;
    // 修复（F133，modularity.md §7 的 P5）：以前 ID 取读配置之前的缺省库路径、在开端口之前就报 `checking`，配置或环境变量
    // 改了 `storage.sessionDbPath` 时 ID 不是实际打开的库。ID 又不能在首帧之后改（Host 丢掉 ID 变了的帧，app-protocol.md
    // §4.2），所以首帧等库的位置定了再发：ID 取会话库所用的同一份存储位置。
    let state = Startup::new(
        context.clone(),
        database_id(opened.paths.database().as_path()),
    );
    state.checking().await;
    // 会话库：打开一次（node-store 在打开连接时校验迁移）；失败时报 failed，不再往下组装。
    if let Err(error) = opened
        .store
        .list(SessionFilter {
            workspace: opened.workspace.clone(),
            limit: 1,
        })
        .await
    {
        tracing::warn!(event = "app.storage.failed", error = %error, "opening the session database failed");
        state.failed("open_failed").await;
        anyhow::bail!("opening the session database failed: {error}");
    }
    let ported = assemble::ports(&spec, facts, opened, (clock, model_io)).await?;
    let built = assemble::kernel(&spec, facts, ported, clock).await?;
    state.ready().await;
    let defaults = Defaults {
        // App 模式组装时已等缺省模型定下（`Controller::app` 的 `wait_model`；performance.md §12.3 只改 TUI）。
        selection: built.selection.current(),
        mode: built.mode,
        environment: EnvironmentId::new(LOCAL_ENVIRONMENT),
    };
    let input = tokio::io::BufReader::new(tokio::io::stdin());
    // 读到 Host 关掉输入或收到退出信号时结束（Host 回收整棵进程树）。
    tokio::select! {
        result = rust_cli_protocol_zcode::serve(input, context, built.kernel.clone(), defaults, spawner, Options::default()) => {
            result?;
        }
        () = stop.token.cancelled() => {}
    }
    Ok(())
}

#[cfg(test)]
#[path = "app_server_tests.rs"]
mod tests;
