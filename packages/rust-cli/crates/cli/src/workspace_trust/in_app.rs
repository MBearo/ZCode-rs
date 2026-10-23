//! 应用内切到别的项目时的信任判定（修复线 F29，docs/specs/rust-cli/workspace-trust.md §2.8）：与启动时同一段判定
//! （[`super::decide`]），不带项目授权的后备询问；同意后写记录。home 与写失败的目录只在本进程有效，记在这里。
use std::sync::{Arc, Mutex, PoisonError};

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use rust_cli_kernel_api::env::{Environment, WorkspaceRef};
use rust_cli_kernel_api::store::SessionStore;
use rust_cli_kernel_api::workspace_trust::TrustKey;
use rust_cli_tui::trust::TrustPrompt;

use super::{Decision, Inputs, decide, record};

pub(crate) struct InAppTrust {
    pub env: Arc<dyn Environment>,
    pub store: Arc<dyn SessionStore>,
    /// 本进程里同意过的键（home 不写记录；写失败时本次仍算信任）。
    pub accepted: Arc<Mutex<Vec<TrustKey>>>,
}

impl InAppTrust {
    async fn decide(&self, workspace: &WorkspaceRef) -> Decision {
        decide(Inputs {
            env: self.env.as_ref(),
            store: self.store.as_ref(),
            workspace,
            grants: None,
        })
        .await
    }

    fn accepted(&self) -> std::sync::MutexGuard<'_, Vec<TrustKey>> {
        self.accepted.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl rust_cli_tui::services::WorkspaceTrust for InAppTrust {
    fn check(&self, workspace: WorkspaceRef) -> BoxFuture<'static, Option<TrustPrompt>> {
        let this = Self {
            env: self.env.clone(),
            store: self.store.clone(),
            accepted: self.accepted.clone(),
        };
        async move {
            match this.decide(&workspace).await {
                Decision::Continue => None,
                Decision::Ask { key, .. } if this.accepted().contains(&key) => None,
                Decision::Ask { prompt, .. } => Some(prompt),
            }
        }
        .boxed()
    }

    fn accept(&self, workspace: WorkspaceRef) -> BoxFuture<'static, ()> {
        let this = Self {
            env: self.env.clone(),
            store: self.store.clone(),
            accepted: self.accepted.clone(),
        };
        async move {
            if let Decision::Ask { key, home, .. } = this.decide(&workspace).await {
                record(this.store.as_ref(), &key, home).await;
                this.accepted().push(key);
            }
        }
        .boxed()
    }
}
