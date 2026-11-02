//! 判决链的替身（`PermissionGate` 的方法都是必需的，F125）。

use async_trait::async_trait;
use serde_json::Value;

use crate::env::WorkspaceRef;
use crate::orchestration::PermissionGate;
use crate::permission::PermissionDecision;
use crate::tool::{Tool, ToolUseContext};

/// 每次判决都给同一个结论的判决链：hook 放行后的检查没有意见，“总是允许”落库后什么都不做。
pub struct FixedGate(pub PermissionDecision);

impl FixedGate {
    /// 判决链没有意见（交给工具自身的检查，最后询问）。
    pub fn passthrough() -> Self {
        Self(PermissionDecision::Passthrough)
    }

    /// 一律放行。
    pub fn allow() -> Self {
        Self(PermissionDecision::Allow {
            updated_input: None,
        })
    }
}

#[async_trait]
impl PermissionGate for FixedGate {
    async fn decide(&self, _: &dyn Tool, _: &Value, _: &ToolUseContext) -> PermissionDecision {
        self.0.clone()
    }

    async fn local_rules_committed(&self, _: &WorkspaceRef) {}

    async fn check_rules(
        &self,
        _: &dyn Tool,
        _: &Value,
        _: &ToolUseContext,
    ) -> Option<PermissionDecision> {
        None
    }
}
