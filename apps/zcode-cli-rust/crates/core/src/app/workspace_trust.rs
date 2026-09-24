//! Engine side of project hook trust (Node `WorkspaceHookTrustCoordinator`,
//! `WorkspaceHookRuntimeAdmission` and the admission banner). The engine owns
//! one state per root session; runs read the published admission view.
use super::Engine;
use crate::contract::{ConfigSource, TrustLoad, TrustStorePort, WorkspaceHooks};
use crate::domain::hooks::{
    Registration,
    trust::{self, AdmissionView, Item, Policy, Record},
    workspace::{self, Snapshot},
};
use anyhow::{Context, Result};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use tokio::sync::watch;

/// Engine-wide hook inputs and every session's trust state.
#[derive(Default)]
pub(super) struct HookState {
    /// Hooks from the user config file.
    pub user: Arc<[Registration]>,
    /// The startup `hooks` config and user config path (merged with plugin hooks).
    pub user_config: serde_json::Value,
    pub user_path: String,
    /// Per session: user and plugin hooks when plugins declare any (Node
    /// resolves them once per App; `None`: the session's plugins have none).
    pub plugins: BTreeMap<String, Option<Arc<[Registration]>>>,
    /// App-server only: project hooks go through workspace trust.
    pub ports: Option<TrustPorts>,
    pub sessions: BTreeMap<String, WorkspaceTrust>,
    /// Sessions whose workspace declared no project hooks.
    pub without: BTreeSet<String>,
}

#[derive(Clone)]
pub(super) struct TrustPorts {
    pub store: Arc<dyn TrustStorePort>,
    pub config: Arc<dyn ConfigSource>,
    pub app_version: Option<String>,
}

/// One session's view of the workspace hook bundle.
pub(super) struct WorkspaceTrust {
    pub snapshot: Snapshot,
    pub records: Vec<Record>,
    pub corrupt: bool,
    /// Revoked in this process: shown as `revoked` instead of `pending_trust`.
    pub revoked: BTreeSet<(String, String)>,
    /// The store could not be read: every project hook stays blocked.
    pub bootstrap_failed: bool,
    /// Config rebuild pending after a toggle wrote the file.
    pub invalidated: Option<&'static str>,
    pub items: Vec<Item>,
    pub view: watch::Sender<Arc<AdmissionView>>,
    pub review: super::workspace_review::Review,
}

impl WorkspaceTrust {
    /// Node `replacePersistentTrustRecords` after a store write.
    pub fn replace_records(&mut self, records: Vec<Record>) {
        for record in &records {
            self.revoked.remove(&record.key());
        }
        self.records = records;
        self.corrupt = false;
    }
}

impl Engine {
    /// App-server: project hooks are admitted through the shared trust store.
    pub fn with_workspace_trust(
        mut self,
        store: Arc<dyn TrustStorePort>,
        config: Arc<dyn ConfigSource>,
        app_version: Option<String>,
    ) -> Self {
        self.hooks.ports = Some(TrustPorts {
            store,
            config,
            app_version,
        });
        self
    }

    /// Node `activate`: the first run of a session in this process discovers
    /// the bundle, loads the store, evaluates and shows the banner. Later runs
    /// reuse the state.
    pub(super) async fn workspace_hooks(&mut self, id: &str) -> Result<Option<WorkspaceHooks>> {
        let Some(ports) = self.hooks.ports.clone() else {
            return Ok(None);
        };
        if !self.hooks.sessions.contains_key(id) {
            if self.hooks.without.contains(id) {
                return Ok(None);
            }
            let config = ports.config.load().await?;
            let Some(snapshot) =
                workspace::discover(&config, &self.workspace, &self.workspace_path)
            else {
                self.hooks.without.insert(id.into());
                return Ok(None);
            };
            let (records, corrupt, failed) = match ports.store.load().await {
                Ok(TrustLoad::Records(records)) => (records, false, false),
                Ok(TrustLoad::Corrupt) => (vec![], true, false),
                Err(error) => {
                    // 信任存储读取失败不能扩大执行面：项目 hooks 全部阻止，其他 hooks 照常。
                    tracing::warn!(
                        target: "zcode::hooks",
                        event = "workspace_hook.trust_store_failure",
                        error = %format!("{error:#}"),
                        "Workspace Hook Trust store could not be loaded"
                    );
                    (vec![], false, true)
                }
            };
            let run_id = format!("workspace-hook-run:{id}:{}", self.clock.id());
            let trust = WorkspaceTrust {
                snapshot,
                records,
                corrupt,
                revoked: BTreeSet::new(),
                bootstrap_failed: failed,
                invalidated: None,
                items: vec![],
                view: watch::channel(Arc::default()).0,
                review: super::workspace_review::Review::new(run_id),
            };
            self.hooks.sessions.insert(id.into(), trust);
            self.refresh_trust(id)?;
            self.publish(id, vec![])?;
        }
        let trust = &self.hooks.sessions[id];
        Ok(Some(WorkspaceHooks {
            registrations: workspace::registrations(&trust.snapshot),
            view: trust.view.subscribe(),
        }))
    }

    /// Re-evaluates a session's hooks, republishes the admission view and sets
    /// the banner (Node `emitAdmissionState` / `WorkspaceHookAdmissionUpdated`).
    pub(super) fn refresh_trust(&mut self, id: &str) -> Result<()> {
        let trust = self
            .hooks
            .sessions
            .get_mut(id)
            .context("Workspace hooks unavailable")?;
        trust.items = trust::evaluate(
            &trust.snapshot,
            Policy::UserDecides,
            &trust.records,
            &trust.revoked,
            trust.corrupt,
        );
        let blocked = trust.invalidated.or(trust
            .bootstrap_failed
            .then_some("workspace_hooks_trust_store_corrupt"));
        trust
            .view
            .send_replace(Arc::new(AdmissionView::new(&trust.items, blocked)));
        let pending = if blocked.is_some() {
            0
        } else {
            trust::pending_count(&trust.items)
        };
        let banner = (pending > 0).then(|| {
            json!({"pendingCount":pending,"bundleDigest":trust.snapshot.bundle_digest,
                "workspaceIdentity":trust.snapshot.workspace_identity})
        });
        let session = self.sessions.get_mut(id).context("Session unavailable")?;
        session.runtime.workspace_hook_admission = banner;
        session.revision += 1;
        Ok(())
    }

    /// Sessions of this workspace reload the store after a grant from outside
    /// a review (Node `reloadWorkspaceHookTrust`).
    pub(super) async fn reload_trust(&mut self, ports: &TrustPorts) -> Result<()> {
        let ids: Vec<String> = self.hooks.sessions.keys().cloned().collect();
        if ids.is_empty() {
            return Ok(());
        }
        let loaded = ports.store.load().await;
        for id in ids {
            let trust = self.hooks.sessions.get_mut(&id).unwrap();
            match &loaded {
                Ok(TrustLoad::Records(records)) => {
                    trust.bootstrap_failed = false;
                    trust.replace_records(records.clone());
                }
                Ok(TrustLoad::Corrupt) => trust.corrupt = true,
                Err(_) => trust.bootstrap_failed = true,
            }
            if self.sessions.contains_key(&id) {
                self.refresh_trust(&id)?;
                self.publish(&id, vec![])?;
            }
        }
        Ok(())
    }
}
