//! controller 所在机器的服务（docs/specs/rust-cli/deployment.md §7、§10）：取网页（WebFetch）。
//!
//! 这些操作从 controller 出网，不跟着会话的主 env 走；所以不在 [`crate::machine::MachineEnv`] 里，执行器不实现。
//! Git 与 worktree 在主 env 上（[`crate::machine::Git`]）。本机的实现是 env-local；后加的方法默认返回 `Unsupported`。

use std::sync::Arc;

use async_trait::async_trait;

use crate::env::{EnvError, WorkspaceRef};
use crate::env_web::{FetchUrl, FetchedUrl};
use crate::machine::not_provided;

/// 取一个网页。
#[async_trait]
pub trait Fetch: Send + Sync {
    /// 取一个网页（read，WebFetch；docs/specs/rust-cli/tools-more.md §4）：一次 GET，不跟随重定向。
    async fn fetch_url(
        &self,
        _workspace: &WorkspaceRef,
        _request: FetchUrl,
    ) -> Result<FetchedUrl, EnvError> {
        Err(not_provided("fetch_url"))
    }
}

/// controller 所在机器的服务。只经 [`ControllerServices::new`] 建。
#[derive(Clone)]
pub struct ControllerServices {
    fetch: Arc<dyn Fetch>,
}

impl ControllerServices {
    /// `services` 实现取网页（本机是 env-local 的 `LocalEnvironment`）。
    pub fn new<S>(services: Arc<S>) -> Self
    where
        S: Fetch + 'static,
    {
        Self { fetch: services }
    }

    pub fn fetch(&self) -> &Arc<dyn Fetch> {
        &self.fetch
    }
}
