//! Workspace hook trust (Node `workspace-hook-trust-store-file.ts`,
//! `workspace-hook-trust-evaluation.ts`, `workspace-hook-trust-records.ts`
//! and `workspace-hook-review-request.ts`).
pub use super::review::{REVIEW_TIMEOUT_MS, ReviewHost, review_request};
pub use super::trust_record::{Record, parse_record, parse_store, store_content};
use super::workspace::{Entry, Snapshot};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrustState {
    PendingTrust,
    TrustedPersistent,
    BlockedUntrusted,
    BlockedPolicy,
    Revoked,
    StaleDigest,
}

impl TrustState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PendingTrust => "pending_trust",
            Self::TrustedPersistent => "trusted_persistent",
            Self::BlockedUntrusted => "blocked_untrusted",
            Self::BlockedPolicy => "blocked_policy",
            Self::Revoked => "revoked",
            Self::StaleDigest => "stale_digest",
        }
    }
    /// Node `WORKSPACE_HOOK_STATE_ADMISSION_MAP` admission class.
    pub fn admission(self) -> &'static str {
        match self {
            Self::TrustedPersistent => "admitted",
            Self::BlockedUntrusted | Self::BlockedPolicy => "blocked",
            _ => "pending",
        }
    }
    pub fn reason_code(self) -> &'static str {
        match self {
            Self::PendingTrust => "workspace_hooks_pending_trust",
            Self::TrustedPersistent => "workspace_hooks_trusted_persistent",
            Self::BlockedUntrusted => "workspace_hooks_blocked_untrusted",
            Self::BlockedPolicy => "workspace_hooks_blocked_by_policy",
            Self::Revoked => "workspace_hooks_revoked",
            Self::StaleDigest => "workspace_hook_declaration_changed",
        }
    }
}

/// Node `WorkspaceHookPolicy.mode`; Rust has no policy provider (user decides).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Policy {
    #[default]
    UserDecides,
    Deny,
    AllowTrustedOnly,
}

/// One entry's evaluated admission (Node `WorkspaceHookEffectiveState`).
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub review_item_id: String,
    pub digest: String,
    pub trust_state: TrustState,
    pub configured_enabled: bool,
    pub effective_runnable: bool,
}

/// Node `evaluateWorkspaceHookEntry` for every entry of `snapshot`.
pub fn evaluate(
    snapshot: &Snapshot,
    policy: Policy,
    records: &[Record],
    revoked: &BTreeSet<(String, String)>,
    corrupt: bool,
) -> Vec<Item> {
    let identity = &snapshot.workspace_identity;
    snapshot
        .hooks
        .iter()
        .map(|entry| {
            let key = (identity.clone(), entry.digest.clone());
            let persistent = records.iter().any(|r| r.key() == key);
            let state = match policy {
                Policy::Deny => TrustState::BlockedPolicy,
                _ if corrupt => TrustState::BlockedUntrusted,
                Policy::AllowTrustedOnly if persistent => TrustState::TrustedPersistent,
                Policy::AllowTrustedOnly => TrustState::BlockedPolicy,
                _ if persistent => TrustState::TrustedPersistent,
                _ if revoked.contains(&key) => TrustState::Revoked,
                _ if stale(snapshot, entry, records) => TrustState::StaleDigest,
                _ => TrustState::PendingTrust,
            };
            Item {
                review_item_id: entry.review_item_id.clone(),
                digest: entry.digest.clone(),
                trust_state: state,
                configured_enabled: entry.configured_enabled,
                effective_runnable: state.admission() == "admitted"
                    && entry.configured_enabled
                    && policy != Policy::Deny,
            }
        })
        .collect()
}

/// Node `hasStaleSlotRecord`: a grant for the same slot of another declaration.
fn stale(snapshot: &Snapshot, entry: &Entry, records: &[Record]) -> bool {
    let Some(source) = snapshot.source_files.get(entry.source_file_index) else {
        return false;
    };
    records.iter().any(|r| {
        r.workspace_identity == snapshot.workspace_identity
            && r.hook_declaration_digest != entry.digest
            && r.event_at_grant == entry.event.as_str()
            && r.source_path_at_grant == entry.source_relative_path
            && r.source_discovery_order_at_grant == Some(source.discovery_order as u64)
            && r.matcher_at_grant.as_ref() == Some(&entry.matcher)
            && r.matcher_index_at_grant == Some(entry.matcher_index as u64)
            && r.hook_index_at_grant == Some(entry.hook_index as u64)
    })
}

/// Node admission `pendingCount`: enabled declarations still pending.
pub fn pending_count(items: &[Item]) -> usize {
    items
        .iter()
        .filter(|i| i.configured_enabled && i.trust_state.admission() == "pending")
        .count()
}

/// Node `createWorkspaceHookTrustRecords`: `Err` names an unknown item.
pub fn grant_records(
    snapshot: &Snapshot,
    review_item_ids: &[String],
    granted_at: &str,
    app_version: Option<&str>,
) -> Result<Vec<Record>, String> {
    let mut seen = BTreeSet::new();
    review_item_ids
        .iter()
        .filter(|id| seen.insert(id.as_str()))
        .map(|id| {
            let entry = snapshot
                .hooks
                .iter()
                .find(|e| &e.review_item_id == id)
                .ok_or_else(|| format!("Unknown Workspace Hook review item: {id}"))?;
            let source = snapshot
                .source_files
                .get(entry.source_file_index)
                .ok_or_else(|| format!("Workspace Hook source is missing for {id}"))?;
            Ok(Record {
                workspace_identity: snapshot.workspace_identity.clone(),
                hook_declaration_digest: entry.digest.clone(),
                digest_algorithm: "sha256".into(),
                decision: "trusted".into(),
                granted_at: granted_at.into(),
                last_used_at: None,
                bundle_digest_at_grant: Some(snapshot.bundle_digest.clone()),
                event_at_grant: entry.event.as_str().into(),
                display_command_at_grant: entry.display_command().trim().into(),
                source_path_at_grant: entry.source_relative_path.clone(),
                source_discovery_order_at_grant: Some(source.discovery_order as u64),
                matcher_at_grant: Some(entry.matcher.clone()),
                matcher_index_at_grant: Some(entry.matcher_index as u64),
                hook_index_at_grant: Some(entry.hook_index as u64),
                app_version_at_grant: app_version.map(str::to_owned),
            })
        })
        .collect()
}

/// What a run needs to admit project hooks; the engine republishes it after
/// every trust change (Node `WorkspaceHookRuntimeAdmission` state).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AdmissionView {
    /// Refuses every project hook (trust store failure, config rebuild).
    pub blocked: Option<&'static str>,
    /// reviewItemId → (digest, runnable, reasonCode, configuredEnabled).
    pub items: std::collections::BTreeMap<String, (String, bool, &'static str, bool)>,
}

impl AdmissionView {
    pub fn new(items: &[Item], blocked: Option<&'static str>) -> Self {
        Self {
            blocked,
            items: items
                .iter()
                .map(|i| {
                    let value = (
                        i.digest.clone(),
                        i.effective_runnable,
                        i.trust_state.reason_code(),
                        i.configured_enabled,
                    );
                    (i.review_item_id.clone(), value)
                })
                .collect(),
        }
    }
    /// Node `evaluateDispatch`, decided again before every project hook.
    pub fn admit(&self, review_item_id: &str, digest: &str) -> super::runner::Admission {
        let refuse = |code: &str, skip: bool| super::runner::Admission {
            allowed: false,
            reason_code: Some(code.into()),
            skip_lifecycle: skip,
        };
        let Some((_, runnable, reason, configured)) =
            self.items.get(review_item_id).filter(|(d, ..)| d == digest)
        else {
            return refuse("workspace_hooks_snapshot_mismatch", false);
        };
        if let Some(code) = self.blocked {
            return refuse(code, false);
        }
        if *runnable {
            return super::runner::Admission::ALLOWED;
        }
        refuse(reason, !configured)
    }
}
