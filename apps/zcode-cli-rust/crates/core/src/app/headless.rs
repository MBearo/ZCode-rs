//! Headless (`-p`) runs have no permission client: every ask is denied at
//! once, as Node's `createHeadlessPermissionBroker` delegating to the deny
//! broker (spec rust-m4-headless 3.2).
use super::Engine;
use crate::contract::{PermissionAnswer, PermissionRequest};
use serde_json::Value;
use tokio::sync::oneshot;

impl Engine {
    pub fn with_headless(mut self) -> Self {
        self.headless = true;
        self
    }

    /// `permission.requested` then `permission.resolved` deny; the model reads
    /// the reason verbatim.
    pub(super) fn headless_deny(
        &mut self,
        id: &str,
        call: &Value,
        request: &PermissionRequest,
        reply: oneshot::Sender<PermissionAnswer>,
    ) {
        let tool = call["function"]["name"].as_str().unwrap_or("");
        let interaction = format!("perm_{}", self.clock.id());
        self.legacy_permission_requested(id, &interaction, call, request, false);
        let answer = PermissionAnswer::Deny {
            message: format!("No permission client configured for {tool}"),
            preserve: true,
        };
        let call_id = call["id"].as_str().unwrap_or("");
        self.legacy_permission_resolved(id, call_id, &interaction, &answer);
        let _ = reply.send(answer);
    }
}
