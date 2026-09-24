//! Engine side of hooks: lifecycle events as `hookInvocation` rows (and the
//! UserPromptSubmit `lastError`), PermissionRequest answers racing the user,
//! and an input taken back out of the history after a prompt block.
use super::{Engine, Event, RunEvent};
use crate::contract::PermissionAnswer;
use crate::domain::hooks::{
    projection,
    runner::{Kind, Lifecycle},
};
use crate::domain::permission::{self as policy, Update};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::oneshot;

impl Engine {
    /// Hook events of a run; any other event is handed back. Lifecycle events
    /// apply even after the run was cancelled or ended (cancelled hooks close
    /// their rows, background hooks finish late); the others need the live run.
    pub(super) async fn hook_side(&mut self, event: RunEvent) -> Result<Option<RunEvent>> {
        let RunEvent {
            session_id: id,
            run_id,
            event,
        } = event;
        if let Event::Hook(lifecycle) = event {
            self.hook_event(&id, &run_id, lifecycle).await?;
            return Ok(None);
        }
        if !matches!(
            event,
            Event::PermissionHook { .. }
                | Event::PromptBlocked { .. }
                | Event::WorkspaceHooks { .. }
        ) {
            return Ok(Some(RunEvent {
                session_id: id,
                run_id,
                event,
            }));
        }
        let live = self
            .active
            .get(&id)
            .filter(|a| a.run_id == run_id && !a.cancel.is_cancelled())
            .map(|a| a.turn_id.clone());
        let Some(turn) = live else {
            return Ok(None);
        };
        match event {
            Event::PermissionHook {
                call_id,
                answer,
                updates,
                accepted,
            } => {
                self.hook_permission(&id, &call_id, answer, updates, accepted)
                    .await?
            }
            Event::PromptBlocked { committed } => {
                self.prompt_blocked(&id, &turn, committed).await?
            }
            Event::WorkspaceHooks { reply } => {
                let hooks = self.workspace_hooks(&id).await.unwrap_or_else(|error| {
                    // 发现或读取失败时本轮不带项目 hooks（Node 的会话创建会失败，这里不中断会话）。
                    tracing::warn!(
                        target: "zcode::hooks",
                        event = "workspace_hook.discovery_failed",
                        error = %format!("{error:#}"),
                        "Workspace hooks could not be discovered"
                    );
                    None
                });
                let _ = reply.send(hooks);
            }
            _ => unreachable!("not a hook event"),
        }
        Ok(None)
    }

    /// Hooks from the config `hooks` section (Node `createConfiguredHookRunner`),
    /// read once at startup; `user_path` is the user config file declaring them.
    pub fn with_hooks(mut self, hooks: &Value, user_path: &str) -> Self {
        self.hooks.user = crate::domain::hooks::registrations(hooks, Some(user_path)).into();
        self
    }

    /// A lifecycle event of `run_id`. Background hooks may end after their
    /// run: such events only update rows that already exist.
    pub(super) async fn hook_event(
        &mut self,
        id: &str,
        run_id: &str,
        event: Lifecycle,
    ) -> Result<()> {
        let now = self.clock.now();
        let turn = self
            .active
            .get(id)
            .filter(|a| a.run_id == run_id)
            .map(|a| a.turn_id.clone());
        let Some(s) = self.sessions.get_mut(id) else {
            return Ok(());
        };
        let invocation = event.payload["hookInvocationId"].as_str().unwrap_or("");
        let position = s
            .rows
            .iter()
            .rposition(|r| r["kind"] == "hookInvocation" && r["hookInvocationId"] == invocation);
        let existing = position.map(|i| &s.rows[i]);
        let Some(content) = projection::invocation(existing, event.kind, &event.payload, now)
        else {
            return Ok(());
        };
        let (op, row) = match position {
            Some(index) => {
                merge(&mut s.rows[index], content);
                ("row.upserted", s.rows[index].clone())
            }
            None => {
                let Some(turn) = turn else {
                    return Ok(());
                };
                let mut row = s.row("hookInvocation", &turn, invocation, now);
                merge(&mut row, content);
                s.rows.push(row.clone());
                ("row.appended", row)
            }
        };
        let trace = s.runtime_trace.clone().unwrap_or_default();
        if let Some(error) = projection::block_error(event.kind, &event.payload, &row, now, &trace)
        {
            s.last_error = Some(error);
        }
        s.revision += 1;
        s.updated_at = now;
        self.publish(id, vec![json!({"op":op,"row":row})])?;
        // started 只发布不落盘：终态事件会连同这一行一起持久化，每个 hook 少一次写盘；
        // 进程在两者之间退出时，冷恢复不会出现停在运行中的 hook 行。
        if event.kind == Kind::Started {
            return Ok(());
        }
        self.persist(id, None).await
    }

    /// PermissionRequest hooks answered first. The prompt is resolved like a
    /// user answer (rows, project rules) only if it is still pending.
    pub(super) async fn hook_permission(
        &mut self,
        id: &str,
        call: &str,
        mut answer: PermissionAnswer,
        updates: Vec<Update>,
        accepted: oneshot::Sender<bool>,
    ) -> Result<()> {
        let Some(interaction) = self.waiters.permission_for_call(id, call) else {
            let _ = accepted.send(false);
            return Ok(());
        };
        let (owner, host) = {
            let wait = self.waiters.permission(&interaction).unwrap();
            (wait.owner.clone(), wait.host.clone())
        };
        if !updates.is_empty() {
            // 与 Node permission-flow 一致：hook 给出的 permissionUpdates 写入项目规则；
            // 写入失败时交互仍解决，工具以存储错误失败。
            let rules = policy::apply_updates(&self.permissions.project_rules, &updates);
            let value = serde_json::to_value(&rules)?;
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
                        "Project permission rules from a hook were not saved"
                    );
                    answer = PermissionAnswer::Fail(
                        "Failed to persist project permission update".into(),
                    );
                }
            }
            self.refresh_permissions();
        }
        let deltas = self.settle_permission(&host, &owner, call, &interaction, &answer)?;
        let s = self
            .sessions
            .get_mut(&host)
            .context("Session unavailable")?;
        s.revision += 1;
        s.updated_at = self.clock.now();
        self.publish(&host, deltas)?;
        self.persist(&host, None).await?;
        if let Some(wait) = self.waiters.take_permission(&interaction) {
            let _ = wait.reply.send(answer);
        }
        let _ = accepted.send(true);
        Ok(())
    }

    /// UserPromptSubmit blocked the turn's input: the model never sees it.
    /// Its rows stay (Node keeps the user row and ends the turn successfully).
    pub(super) async fn prompt_blocked(
        &mut self,
        id: &str,
        turn: &str,
        committed: oneshot::Sender<()>,
    ) -> Result<()> {
        let s = self.sessions.get_mut(id).context("Session unavailable")?;
        let boundary = s
            .history
            .inputs
            .iter()
            .rev()
            .find(|input| input.turn == turn)
            .map(|input| input.message);
        if let Some(boundary) = boundary.filter(|b| *b >= s.context.offset) {
            s.truncate_messages(boundary);
            s.revision += 1;
            s.updated_at = self.clock.now();
            self.persist(id, None).await?;
        }
        let _ = committed.send(());
        Ok(())
    }
}

/// Node `{...existingRow, ...content}`.
fn merge(row: &mut Value, content: Value) {
    if let (Some(row), Value::Object(content)) = (row.as_object_mut(), content) {
        row.extend(content);
    }
}
