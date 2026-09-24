//! Resolving permission prompts (Node `interaction-broker.ts` answer mapping,
//! `permission-flow.ts` rule persistence, `permission-full-access.ts`).
use super::Engine;
use crate::{
    contract::PermissionAnswer,
    domain::{
        execution::Mode,
        permission::{self as policy, Behavior, Rule, Update},
        protocol::Command,
    },
};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::sync::Arc;

/// Node `permission-flow.ts` tool error when the allowAlways rules cannot be saved.
const PERSIST_FAILED: &str = "Failed to persist project permission update";

/// What the chosen option does besides answering the tool.
enum Grant {
    None,
    Project(Vec<Update>),
    Session(String),
}

/// Node `mapV4AnswerToPermissionResponse` for ordinary permission prompts.
fn map_answer(tool: &str, suggestions: &[Update], answer: &Value) -> (PermissionAnswer, Grant) {
    let option = answer["optionId"].as_str();
    let feedback = answer["freeText"]
        .as_str()
        .map(str::trim)
        .filter(|f| !f.is_empty());
    let deny = || PermissionAnswer::Deny {
        message: policy::denied_content(feedback),
        preserve: feedback.is_some(),
    };
    if matches!(tool, "CreateWorkflow" | "AmendWorkflow")
        && option == Some("workflowRefine")
        && let Some(feedback) = feedback
    {
        return (
            PermissionAnswer::Deny {
                message: feedback.into(),
                preserve: true,
            },
            Grant::None,
        );
    }
    match option {
        Some("deny") => (deny(), Grant::None),
        Some("allowSession") => (PermissionAnswer::Allow, Grant::Session(tool.into())),
        Some("allow_once") | Some("allowOnce") => (PermissionAnswer::Allow, Grant::None),
        // allowAlways 取 allow_always 选项的建议规则；会话级选项策略下没有该选项，按拒绝处理。
        Some("allow_project") | Some("allowAlways") => {
            let updates = if suggestions.is_empty() {
                policy::default_updates(tool, &Value::Null, None)
            } else {
                suggestions.to_vec()
            };
            (PermissionAnswer::Allow, Grant::Project(updates))
        }
        _ => (deny(), Grant::None),
    }
}

impl Engine {
    /// Answers one prompt hosted on session `id`. The owner may be a subagent.
    pub(super) async fn resolve_permission(
        &mut self,
        c: &Command,
        id: &str,
        interaction: &str,
    ) -> Result<Value> {
        let answer = &c.payload["answer"];
        if answer["optionId"] == "fullAccess" {
            return self.grant_full_access(c, id, interaction).await;
        }
        let (owner, tool, call, suggestions, policy_opt) = {
            let wait = self
                .waiters
                .permission(interaction)
                .context("Permission missing")?;
            let entry = self.sessions[id]
                .pending
                .iter()
                .find(|p| p["interactionId"] == interaction);
            // 选项策略决定 allowAlways 是否存在；投影中没有 allowAlways 时它等同拒绝。
            let has_project = entry.is_some_and(|e| {
                e["payload"]["options"]
                    .as_array()
                    .is_some_and(|o| o.iter().any(|o| o["optionId"] == "allowAlways"))
            });
            (
                wait.owner.clone(),
                wait.tool.clone(),
                wait.call.clone(),
                wait.suggestions.clone(),
                has_project,
            )
        };
        if tool == crate::domain::plan_mode::EXIT {
            return self
                .resolve_plan_approval(c, id, interaction, &owner, &call)
                .await;
        }
        let (mut decision, grant) = map_answer(&tool, &suggestions, answer);
        let grant = match grant {
            Grant::Project(_) if !policy_opt => {
                decision = PermissionAnswer::Deny {
                    message: policy::denied_content(None),
                    preserve: false,
                };
                Grant::None
            }
            other => other,
        };
        match grant {
            Grant::Project(updates) => {
                let rules = policy::apply_updates(&self.permissions.project_rules, &updates);
                let value = serde_json::to_value(&rules)?;
                // 与 Node permission-flow 一致：规则写入失败时交互仍按允许解决，但工具不执行，
                // 模型收到存储错误；内存规则保持旧值，避免重启后授权凭空消失。
                match self
                    .store
                    .save_project_setting(&self.workspace, "permission", "ruleset", &value)
                    .await
                {
                    Ok(()) => self.permissions.project_rules = Arc::new(rules),
                    Err(error) => {
                        tracing::warn!(
                            target: "zcode::permission",
                            event = "permission.project_rules_write_failed",
                            error = %error,
                            "Project permission rules were not saved"
                        );
                        decision = PermissionAnswer::Fail(PERSIST_FAILED.into());
                    }
                }
            }
            Grant::Session(tool) => {
                let root = self.root_session(&owner);
                let current = self
                    .permissions
                    .session_rules
                    .get(&root)
                    .cloned()
                    .unwrap_or_default();
                let update = Update {
                    kind: "addRules".into(),
                    behavior: Behavior::Allow,
                    rules: vec![Rule {
                        tool_name: tool,
                        rule_content: None,
                    }],
                };
                let rules = policy::apply_updates(&current, &[update]);
                self.permissions.session_rules.insert(root, Arc::new(rules));
            }
            Grant::None => {}
        }
        self.refresh_permissions();
        let deltas = self.settle_permission(id, &owner, &call, interaction, &decision)?;
        let ack = self.commit_interaction(c, deltas).await?;
        if let Some(wait) = self.waiters.take_permission(interaction) {
            let _ = wait.reply.send(decision);
        }
        Ok(ack)
    }

    /// Node plan approval: approve runs ExitPlanMode; feedback is steered into the
    /// turn at the next step boundary; any other answer denies and stops the turn.
    async fn resolve_plan_approval(
        &mut self,
        c: &Command,
        id: &str,
        interaction: &str,
        owner: &str,
        call: &str,
    ) -> Result<Value> {
        use crate::domain::plan_mode::{Approval, map_answer};
        let decision = match map_answer(&c.payload["answer"]) {
            Approval::Approve => PermissionAnswer::Allow,
            Approval::Reject(feedback) => {
                let session = self
                    .sessions
                    .get_mut(owner)
                    .context("Session unavailable")?;
                session.runtime.plan_feedback = feedback.clone();
                PermissionAnswer::PlanRejected(feedback)
            }
        };
        let deltas = self.settle_permission(id, owner, call, interaction, &decision)?;
        let ack = self.commit_interaction(c, deltas).await?;
        if let Some(wait) = self.waiters.take_permission(interaction) {
            let _ = wait.reply.send(decision);
        }
        Ok(ack)
    }

    /// Removes the prompt from its host and moves the owner's row on.
    pub(super) fn settle_permission(
        &mut self,
        host: &str,
        owner: &str,
        call: &str,
        interaction: &str,
        decision: &PermissionAnswer,
    ) -> Result<Vec<Value>> {
        self.legacy_permission_resolved(owner, call, interaction, decision);
        self.sessions
            .get_mut(host)
            .context("Session unavailable")?
            .pending
            .retain(|p| p["interactionId"] != interaction);
        let s = self
            .sessions
            .get_mut(owner)
            .context("Session unavailable")?;
        let Some(row) = s.rows.iter_mut().find(|r| r["toolCallId"] == call) else {
            return Ok(vec![]);
        };
        row.as_object_mut().unwrap().remove("approvalInteractionId");
        row["status"] = match decision {
            PermissionAnswer::Allow | PermissionAnswer::Fail(_) => "running",
            PermissionAnswer::Deny { .. } | PermissionAnswer::PlanRejected(_) => "cancelled",
        }
        .into();
        let delta = json!({"op":"row.upserted","row":row});
        if owner == host {
            return Ok(vec![delta]);
        }
        s.revision += 1;
        self.publish(owner, vec![delta])?;
        Ok(vec![])
    }

    /// Node `grantPermissionFullAccess`: the session and its queued inputs switch
    /// to yolo in one commit (plan stays as is), then the prompt resolves as
    /// allow once. Only the current prompt is answered (D6).
    async fn grant_full_access(
        &mut self,
        c: &Command,
        id: &str,
        interaction: &str,
    ) -> Result<Value> {
        let s = self.sessions.get(id).context("Session unavailable")?;
        let offered = s.pending.iter().any(|p| {
            p["interactionId"] == interaction && p["payload"]["fullAccessOption"].is_object()
        });
        if !offered {
            let mut ack = c.ack("failed", s.revision, Some("fault.command.executionFailed"));
            ack["message"] = "Full access is not supported for this interaction".into();
            return Ok(ack);
        }
        if s.queued_now.is_some() {
            let mut ack = c.ack("failed", s.revision, Some("fault.command.executionFailed"));
            ack["message"] = "Queue mutation is busy; retry approval".into();
            return Ok(ack);
        }
        let (owner, call) = {
            let wait = self
                .waiters
                .permission(interaction)
                .context("Permission missing")?;
            (wait.owner.clone(), wait.call.clone())
        };
        let s = self.sessions.get_mut(id).unwrap();
        s.mode = Mode::Yolo;
        s.permission_grant = Some(interaction.into());
        for item in &mut s.queue {
            item["mode"] = "yolo".into();
        }
        self.refresh_permissions();
        let decision = PermissionAnswer::Allow;
        let deltas = self.settle_permission(id, &owner, &call, interaction, &decision)?;
        let ack = self.commit_interaction(c, deltas).await?;
        if let Some(wait) = self.waiters.take_permission(interaction) {
            let _ = wait.reply.send(decision);
        }
        Ok(ack)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_map_like_the_node_broker() {
        let (answer, _) = map_answer("Write", &[], &json!({"optionId":"allowOnce"}));
        assert_eq!(answer, PermissionAnswer::Allow);
        let (answer, grant) = map_answer("Write", &[], &json!({"optionId":"allowAlways"}));
        assert_eq!(answer, PermissionAnswer::Allow);
        assert!(matches!(grant, Grant::Project(_)));
        let (answer, _) = map_answer("Write", &[], &json!({"optionId":"deny","freeText":" no "}));
        assert_eq!(
            answer,
            PermissionAnswer::Deny {
                message: policy::denied_content(Some("no")),
                preserve: true
            }
        );
        let (answer, _) = map_answer("Write", &[], &json!({}));
        assert!(matches!(
            answer,
            PermissionAnswer::Deny {
                preserve: false,
                ..
            }
        ));
        let (_, grant) = map_answer("CreateWorkflow", &[], &json!({"optionId":"allowSession"}));
        assert!(matches!(grant, Grant::Session(tool) if tool == "CreateWorkflow"));
    }
}
