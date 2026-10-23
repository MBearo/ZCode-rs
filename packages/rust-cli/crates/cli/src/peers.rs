//! 本机跨会话通信的组装（docs/specs/rust-cli/cross-session.md §2）：登记目录取 `StoragePaths::peers`，socket 路径太长时
//! 退回本用户私有的临时目录；过期时长读 `ZCODE_USER_DIALOG_TIMEOUT_MS`（入口是唯一读环境的地方）；`-p` 的
//! 回执不开回合（passive）。

use std::path::Path;
use std::sync::Arc;

use rust_cli_base::TaskScope;
use rust_cli_kernel_api::clock::{Clock, Ids};
use rust_cli_kernel_api::peers::DEFAULT_HOLD_TIMEOUT_MS;
use rust_cli_paths::StoragePaths;
use rust_cli_peers::{LocalPeers, PeerConfig};
use rust_cli_runtime::PeerDeps;

/// 进程级的跨会话：端口、交给会话的配置与它自己的任务作用域（收件箱、写登记）。
pub(crate) struct Peers {
    pub(crate) local: Arc<LocalPeers>,
    pub(crate) deps: PeerDeps,
    scope: TaskScope,
}

/// `ZCODE_USER_DIALOG_TIMEOUT_MS`（毫秒）；没有或不是数字时缺省 300000。
pub(crate) fn hold_timeout(env: &[(String, String)]) -> u64 {
    rust_cli_base::secret::env_var(env, "ZCODE_USER_DIALOG_TIMEOUT_MS")
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_HOLD_TIMEOUT_MS)
}

pub(crate) struct Startup<'a> {
    pub(crate) paths: &'a StoragePaths,
    pub(crate) temp: &'a Path,
    pub(crate) cwd: &'a Path,
    pub(crate) env: &'a [(String, String)],
    pub(crate) version: &'a str,
    pub(crate) print: bool,
}

pub(crate) async fn start(startup: Startup<'_>, clock: Arc<dyn Clock>, ids: Arc<dyn Ids>) -> Peers {
    let scope = TaskScope::root("peers", |panic| {
        tracing::error!(scope = %panic.scope, task = panic.task, message = %panic.message, "task panicked");
    });
    // socket 路径太长时依次退回：TMPDIR 与 /tmp 下本用户私有的目录（proc 建好并核对属主）。
    let mut fallback_sockets = Vec::new();
    for temp in [startup.temp, Path::new("/tmp")] {
        let dir = rust_cli_proc::private::user_dir(temp);
        if cfg!(unix) && rust_cli_proc::private::ensure(&dir).await.is_ok() {
            fallback_sockets.push(dir.join("peers"));
        }
    }
    let local = LocalPeers::start(PeerConfig {
        registry: startup.paths.peers().as_path().to_path_buf(),
        fallback_sockets,
        pid: std::process::id(),
        cwd: startup.cwd.to_string_lossy().into_owned(),
        version: startup.version.to_owned(),
        clock,
        ids,
        spawner: scope.spawner(),
    })
    .await;
    let local = Arc::new(local);
    let deps = PeerDeps {
        port: local.clone(),
        hold_timeout_ms: hold_timeout(startup.env),
        receipts_wake: !startup.print,
    };
    Peers { local, deps, scope }
}

impl Peers {
    /// 退出：删掉登记与 socket，再关掉自己的任务（还在发的回执最多等一小会儿，750 ms）。
    pub(crate) async fn shutdown(self) {
        self.local.shutdown().await;
        let _ = self
            .scope
            .close(std::time::Duration::from_millis(750))
            .await;
    }
}

#[cfg(test)]
#[path = "peers_tests.rs"]
mod tests;
