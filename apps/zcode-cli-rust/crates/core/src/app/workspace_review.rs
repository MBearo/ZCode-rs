//! Workspace hook review flows and their commands (Node
//! `WorkspaceHookReviewController`, `WorkspaceHookReviewFlowRegistry`, the V4
//! command handlers and `workspace/hooks/trustGrant`).
use super::Engine;
use crate::contract::HookToggle;
use crate::domain::hooks::{
    input::iso_timestamp,
    trust::{self, ReviewHost, TrustState},
    workspace,
};
use crate::domain::protocol::Command;
use anyhow::Result;
use serde_json::{Value, json};

const SUPERSEDED: &str = "workspace_hooks_review_superseded";
const MISMATCH: &str = "workspace_hooks_snapshot_mismatch";
const CORRUPT: &str = "workspace_hooks_trust_store_corrupt";
const UNREADABLE: &str = "workspace_hooks_config_unreadable";
const REBUILD_FAILED: &str = "workspace_hooks_config_rebuild_failed";

/// A session's review flow; generations only grow.
pub(super) struct Review {
    pub flow_id: Option<String>,
    pub generation: u64,
    /// Node review host `runId`, fixed for the session runtime.
    pub run_id: String,
    /// The latest request and whether it is still pending.
    pub current: Option<(Value, bool)>,
}

impl Review {
    pub fn new(run_id: String) -> Self {
        Self {
            flow_id: None,
            generation: 0,
            run_id,
            current: None,
        }
    }
}

type Outcome = std::result::Result<(), &'static str>;

fn texts(value: &Value) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for item in value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if !out.iter().any(|seen| seen == item) {
            out.push(item.into());
        }
    }
    out
}

impl Engine {
    pub(super) async fn workspace_hook_command(&mut self, c: &Command) -> Result<Value> {
        let id = c.session_id.clone().unwrap_or_default();
        let outcome = if c.payload["sessionId"] != id.as_str() {
            Err(MISMATCH)
        } else if !self.hooks.sessions.contains_key(&id) {
            Err(if self.hooks.ports.is_some() {
                "workspace_hooks_require_trust_capable_host"
            } else {
                "workspace_hooks_feature_disabled"
            })
        } else {
            match c.kind.as_str() {
                "requestWorkspaceHookReview" => self.request_review(&id, &c.payload),
                "respondWorkspaceHookReview" => self.respond_review(&id, &c.payload).await,
                "toggleWorkspaceHookReviewItem" => self.toggle_review_item(&id, &c.payload).await,
                _ => self.revoke_trust(&id, &c.payload).await,
            }
        };
        match outcome {
            Ok(()) => self.commit_interaction(c, vec![]).await,
            Err(code) => {
                let revision = self.sessions[&id].revision;
                let mut ack = c.ack("failed", revision, Some(code));
                ack["message"] = format!("Workspace Hook review command rejected: {code}").into();
                // 失败路径也可能已改变状态（toggle 写盘后重建失败），照常发布当前快照。
                self.publish(&id, vec![])?;
                Ok(ack)
            }
        }
    }

    /// Node `openOrReuseFlow`: a pending flow of the same bundle is reused.
    fn open_review(&mut self, id: &str) {
        let now = self.clock.now();
        let interaction = format!("workspace-hook-interaction:{}", self.clock.id());
        let new_flow = format!("workspace-hook-review:{}", self.clock.id());
        let label = workspace_label(&self.workspace_path);
        let trust = self.hooks.sessions.get_mut(id).unwrap();
        if let Some((request, true)) = &trust.review.current
            && request["bundleDigest"] == trust.snapshot.bundle_digest.as_str()
        {
            return;
        }
        let flow = trust.review.flow_id.get_or_insert(new_flow).clone();
        trust.review.generation += 1;
        let host = ReviewHost {
            session_id: id,
            run_id: &trust.review.run_id,
            workspace_label: &label,
            remote_session_id: None,
        };
        let generation = trust.review.generation;
        let request = trust::review_request(
            &trust.snapshot,
            &trust.items,
            (&flow, generation, &interaction),
            &host,
            now,
        );
        trust.review.current = Some((request.clone(), true));
        self.show_review(id, request);
    }

    fn show_review(&mut self, id: &str, request: Value) {
        let Some(s) = self.sessions.get_mut(id) else {
            return;
        };
        // 同一流程只允许更高 generation 替换当前审查交互。
        s.pending.retain(|p| p["kind"] != "workspaceHookReview");
        s.pending.push(
            json!({"interactionId":request["interactionId"],"kind":"workspaceHookReview",
            "anchorRowId":null,"createdAt":request["createdAt"],"payload":request}),
        );
    }

    /// Ends the current flow (resolved, cancelled, timed out) and hides it.
    pub(super) fn settle_review(&mut self, id: &str) {
        let Some(trust) = self.hooks.sessions.get_mut(id) else {
            return;
        };
        let Some((request, pending)) = trust.review.current.as_mut() else {
            return;
        };
        *pending = false;
        let interaction = request["interactionId"].clone();
        if let Some(s) = self.sessions.get_mut(id) {
            s.pending.retain(|p| {
                !(p["kind"] == "workspaceHookReview" && p["interactionId"] == interaction)
            });
        }
    }

    /// Node `refreshPendingFlow`: a pending flow is superseded by the next
    /// generation; without one, a new flow opens only if items are pending.
    fn refresh_review(&mut self, id: &str) {
        let trust = &self.hooks.sessions[id];
        if !matches!(trust.review.current, Some((_, true))) {
            if trust
                .items
                .iter()
                .any(|i| i.trust_state.admission() == "pending")
            {
                self.open_review(id);
            }
            return;
        }
        // 先让旧请求失效，再按同一流程打开下一 generation。
        self.hooks.sessions.get_mut(id).unwrap().review.current = None;
        self.open_review(id);
        let trust = &self.hooks.sessions[id];
        if trust
            .review
            .current
            .as_ref()
            .is_some_and(|(r, _)| r["summary"]["pendingCount"] == 0)
        {
            self.settle_review(id);
        }
    }

    /// Node registry `validate`: the command must target the pending
    /// generation of this snapshot and name known items.
    fn validate_target(&self, id: &str, p: &Value, items: &[String]) -> Outcome {
        let Some((request, true)) = &self.hooks.sessions[id].review.current else {
            return Err(SUPERSEDED);
        };
        let same = |key: &str| request.get(key) == p.get(key);
        if !same("reviewFlowId")
            || request["generation"].as_u64() != p["generation"].as_u64()
            || !same("interactionId")
        {
            return Err(SUPERSEDED);
        }
        let keys = [
            "sessionId",
            "taskId",
            "runId",
            "remoteSessionId",
            "workspaceIdentity",
            "bundleDigest",
        ];
        if !keys.iter().all(|k| same(k)) {
            return Err(MISMATCH);
        }
        let known = request["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|i| i["reviewItemId"].as_str());
        let known: Vec<&str> = known.collect();
        if items.iter().any(|i| !known.contains(&i.as_str())) {
            return Err(MISMATCH);
        }
        Ok(())
    }

    fn matches_snapshot(&self, id: &str, p: &Value) -> bool {
        let snapshot = &self.hooks.sessions[id].snapshot;
        p["workspaceIdentity"] == snapshot.workspace_identity.as_str()
            && p["bundleDigest"] == snapshot.bundle_digest.as_str()
    }

    /// Node `requestReview`: opens (or reuses) a flow when items are pending.
    fn request_review(&mut self, id: &str, p: &Value) -> Outcome {
        if !self.matches_snapshot(id, p) {
            return Err(MISMATCH);
        }
        let trust = &self.hooks.sessions[id];
        if !trust
            .items
            .iter()
            .any(|i| i.trust_state.admission() == "pending")
        {
            if trust
                .items
                .iter()
                .any(|i| i.trust_state == TrustState::BlockedPolicy)
            {
                return Err("workspace_hooks_blocked_by_policy");
            }
            return if trust.corrupt { Err(CORRUPT) } else { Ok(()) };
        }
        self.open_review(id);
        Ok(())
    }

    async fn respond_review(&mut self, id: &str, p: &Value) -> Outcome {
        let items = texts(&p["decision"]["reviewItemIds"]);
        self.validate_target(id, p, &items)?;
        if !self.matches_snapshot(id, p) {
            return Err(MISMATCH);
        }
        let ports = self.hooks.ports.clone().ok_or(CORRUPT)?;
        let granted_at = iso_timestamp(self.clock.now());
        let trust = &self.hooks.sessions[id];
        let records = trust::grant_records(
            &trust.snapshot,
            &items,
            &granted_at,
            ports.app_version.as_deref(),
        )
        .map_err(|_| CORRUPT)?;
        let stored = ports.store.grant(records).await.map_err(|_| CORRUPT)?;
        self.hooks
            .sessions
            .get_mut(id)
            .unwrap()
            .replace_records(stored);
        self.settle_review(id);
        self.refresh_trust(id).map_err(|_| CORRUPT)?;
        self.refresh_review(id);
        Ok(())
    }

    async fn toggle_review_item(&mut self, id: &str, p: &Value) -> Outcome {
        let item = p["reviewItemId"].as_str().unwrap_or("").to_owned();
        self.validate_target(id, p, std::slice::from_ref(&item))?;
        let ports = self.hooks.ports.clone().ok_or(UNREADABLE)?;
        let trust = &self.hooks.sessions[id];
        let entry = trust
            .snapshot
            .hooks
            .iter()
            .find(|e| e.review_item_id == item)
            .ok_or(MISMATCH)?;
        if !entry.editable {
            return Err(MISMATCH);
        }
        let current = self.rediscover(&ports).await?;
        let trust = &self.hooks.sessions[id];
        if current.workspace_identity != trust.snapshot.workspace_identity {
            return Err(MISMATCH);
        }
        if current.bundle_digest != trust.snapshot.bundle_digest {
            return Err("workspace_hooks_bundle_changed");
        }
        let source = &trust.snapshot.source_files[entry.source_file_index];
        let toggle = HookToggle {
            path: source.canonical_path.clone(),
            event: entry.event,
            relative_path: entry.source_relative_path.clone(),
            discovery_order: source.discovery_order,
            matcher_index: entry.matcher_index,
            hook_index: entry.hook_index,
            digest: entry.digest.clone(),
            resolved_timeout_ms: entry.resolved_timeout_ms,
            resolved_max_output_bytes: entry.resolved_max_output_bytes,
            enabled: p["enabled"] == true,
        };
        ports.config.set_workspace_hook_enabled(toggle).await?;
        // 写盘已提交：重建完成前项目 hooks 一律阻止，重建失败时审查以配置错误收口。
        self.hooks.sessions.get_mut(id).unwrap().invalidated = Some(REBUILD_FAILED);
        let _ = self.refresh_trust(id);
        let Ok(next) = self.rediscover(&ports).await else {
            self.settle_review(id);
            return Err(REBUILD_FAILED);
        };
        let trust = self.hooks.sessions.get_mut(id).unwrap();
        trust.snapshot = next;
        trust.invalidated = None;
        self.refresh_trust(id).map_err(|_| REBUILD_FAILED)?;
        self.refresh_review(id);
        Ok(())
    }

    async fn rediscover(
        &self,
        ports: &super::workspace_trust::TrustPorts,
    ) -> std::result::Result<workspace::Snapshot, &'static str> {
        let config = ports.config.load().await.map_err(|_| UNREADABLE)?;
        workspace::discover(&config, &self.workspace, &self.workspace_path).ok_or(MISMATCH)
    }

    /// Node `revoke` (review items) and `revokeCurrent` (declaration digests).
    async fn revoke_trust(&mut self, id: &str, p: &Value) -> Outcome {
        let trust = &self.hooks.sessions[id];
        let digests: Vec<String> = if p.get("hookDeclarationDigests").is_some() {
            let digests = texts(&p["hookDeclarationDigests"]);
            let present = digests
                .iter()
                .all(|d| trust.snapshot.hooks.iter().any(|e| &e.digest == d));
            if p.get("remoteSessionId").is_some()
                || !self.matches_snapshot(id, p)
                || digests.is_empty()
                || !present
            {
                return Err(MISMATCH);
            }
            digests
        } else {
            let items = texts(&p["reviewItemIds"]);
            self.validate_target(id, p, &items)?;
            let entries = trust
                .snapshot
                .hooks
                .iter()
                .filter(|e| items.contains(&e.review_item_id));
            entries.map(|e| e.digest.clone()).collect()
        };
        let ports = self.hooks.ports.clone().ok_or(CORRUPT)?;
        let identity = trust.snapshot.workspace_identity.clone();
        let stored = ports
            .store
            .revoke(&identity, Some(digests.clone()))
            .await
            .map_err(|_| CORRUPT)?;
        let trust = self.hooks.sessions.get_mut(id).unwrap();
        trust.replace_records(stored);
        for digest in digests {
            let key = (identity.clone(), digest);
            trust.records.retain(|r| r.key() != key);
            trust.revoked.insert(key);
        }
        self.refresh_trust(id).map_err(|_| CORRUPT)?;
        self.refresh_review(id);
        Ok(())
    }
}

/// Node `workspaceLabel`: the last path segment.
pub(super) fn workspace_label(path: &str) -> String {
    path.split(['/', '\\'])
        .rfind(|s| !s.is_empty())
        .unwrap_or(path)
        .to_owned()
}
