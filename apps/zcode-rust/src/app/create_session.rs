use super::Engine;
use crate::domain::{protocol::Command, session::Session};
use anyhow::{Result, bail};
use serde_json::{Value, json};
impl Engine {
    pub(super) async fn create(&mut self, c: Command) -> Result<Value> {
        if c.session_id.is_some() || c.payload["workspaceId"] != self.workspace {
            bail!("Workspace identity mismatch");
        }
        let config = self.select(&c.payload["config"], None)?;
        self.validate_selection(&c.payload["config"])?;
        let mut first = c.payload.get("firstInput").cloned();
        if let Some(input) = &first {
            self.validate_input(input)?;
            anyhow::ensure!(
                crate::domain::shared_context::reference(input)?.is_none(),
                "New draft has no imported shared context"
            );
            self.select(input, Some(config.clone()))?;
        }
        let mut session = Session::new(
            self.clock.id(),
            self.workspace.clone(),
            config.provider_id.clone(),
            config.model_id.clone(),
            config.reasoning_level.clone(),
            self.clock.id(),
            self.clock.now(),
        );
        session.workspace_path = Some(self.workspace_path.clone());
        session.workspace_directory = Some(self.workspace_path.clone());
        if let Some(mode) = c.payload["config"]["followupMode"].as_str() {
            session.followup_mode = mode.into();
        }
        let id = session.id.clone();
        if let Some(input) = &mut first {
            let selected = self.select(input, Some(config.clone()))?;
            session.attachments = self.prepare_attachments(&id, input, &selected).await?;
        }
        if let Some(servers) = c.payload.get("mcpServers") {
            self.tools.configure_mcp(&id, servers).await?;
        }
        self.sessions.insert(id.clone(), session);
        self.apply_selection(&id, config)?;
        let mut ack = c.ack("accepted", 0, None);
        ack["result"] = json!({"type":"createSession","sessionId":id});
        let mut turn = None;
        if let Some(input) = first {
            let mut input_command = c.clone();
            input_command.payload = input;
            input_command.session_id = Some(id.clone());
            let (t, input_id) = self.admit_input(&id, &input_command, None)?;
            turn = Some(t);
            ack["result"]["input"] = json!({"delivery":"startNow","inputId":input_id});
        }
        self.publish(&id, vec![])?;
        ack["revisionAtDecision"] = self.sessions[&id].revision.into();
        // draft 仅存内存，第一次输入才持久化；create 的幂等 ACK 与 durable session 一起提交。
        self.sessions.get_mut(&id).unwrap().creation_ack = Some((c.key(), ack.clone()));
        if turn.is_some() {
            self.persist(&id, Some((c.key(), ack.clone()))).await?;
        }
        self.acks.insert(c.key(), ack.clone());
        if let Some(turn) = turn {
            self.start_run(&id, turn)?;
        }
        Ok(ack)
    }
}
