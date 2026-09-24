//! `workspace/hooks/trustGrant` (Settings trust without a session) and the
//! review deadline timer.
use super::Engine;
use crate::contract::{RuntimeError, TrustLoad};
use crate::domain::hooks::{input::iso_timestamp, trust, workspace};
use anyhow::Result;
use serde_json::{Value, json};

type Outcome = std::result::Result<(), &'static str>;
const MISMATCH: &str = "workspace_hooks_snapshot_mismatch";
const CORRUPT: &str = "workspace_hooks_trust_store_corrupt";
const UNREADABLE: &str = "workspace_hooks_config_unreadable";

impl Engine {
    /// `workspace/hooks/trustGrant`: Settings trusts one declaration of the
    /// freshly discovered bundle without a session.
    pub(super) async fn trust_grant(&mut self, p: &Value) -> Result<Value> {
        let path = p["workspace"]["workspacePath"].as_str();
        let (Some(path), Some(bundle), Some(digest)) = (
            path,
            p["bundleDigest"].as_str(),
            p["hookDeclarationDigest"].as_str(),
        ) else {
            return Err(RuntimeError::invalid_params(
                "workspace, bundleDigest and hookDeclarationDigest are required",
            ));
        };
        let identity = p["workspace"]["workspaceIdentity"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(path);
        let outcome = self.grant_declaration(identity, path, bundle, digest).await;
        Ok(match outcome {
            Ok(()) => json!({"accepted": true}),
            Err(code) => json!({"accepted": false, "reasonCode": code}),
        })
    }

    async fn grant_declaration(
        &mut self,
        identity: &str,
        path: &str,
        bundle: &str,
        digest: &str,
    ) -> Outcome {
        let ports = self.hooks.ports.clone().ok_or(UNREADABLE)?;
        // 本进程只服务一个工作区：其他工作区的配置无法在此发现。
        if identity != self.workspace && path != self.workspace_path {
            return Err(UNREADABLE);
        }
        let config = ports.config.load().await.map_err(|_| UNREADABLE)?;
        let snapshot =
            workspace::discover(&config, identity, &self.workspace_path).ok_or(UNREADABLE)?;
        if snapshot.bundle_digest != bundle {
            return Err("workspace_hooks_bundle_changed");
        }
        let entry = snapshot
            .hooks
            .iter()
            .find(|e| e.digest == digest)
            .ok_or(MISMATCH)?;
        match ports.store.load().await {
            Ok(TrustLoad::Records(_)) => {}
            Ok(TrustLoad::Corrupt) => return Err(CORRUPT),
            Err(_) => return Err(UNREADABLE),
        }
        let granted_at = iso_timestamp(self.clock.now());
        let records = trust::grant_records(
            &snapshot,
            std::slice::from_ref(&entry.review_item_id),
            &granted_at,
            ports.app_version.as_deref(),
        )
        .map_err(|_| UNREADABLE)?;
        ports.store.grant(records).await.map_err(|_| UNREADABLE)?;
        self.reload_trust(&ports).await.map_err(|_| UNREADABLE)?;
        Ok(())
    }

    /// The nearest review deadline (engine timer loop).
    pub(super) fn review_deadline(&self) -> Option<u64> {
        self.hooks
            .sessions
            .values()
            .filter_map(|t| match &t.review.current {
                Some((request, true)) => request["deadlineAt"].as_u64(),
                _ => None,
            })
            .min()
    }

    /// Node registry timer: expired flows settle `timed_out` and disappear.
    pub(super) async fn expire_reviews(&mut self) -> Result<()> {
        let now = self.clock.now();
        let due: Vec<String> = self
            .hooks
            .sessions
            .iter()
            .filter(|(_, t)| matches!(&t.review.current, Some((r, true)) if r["deadlineAt"].as_u64().is_some_and(|d| d <= now)))
            .map(|(id, _)| id.clone())
            .collect();
        for id in due {
            self.settle_review(&id);
            if let Some(s) = self.sessions.get_mut(&id) {
                s.revision += 1;
                self.publish(&id, vec![])?;
            }
        }
        Ok(())
    }
}
