use super::Engine;
use crate::{
    contract::{ModelIdentity, StorageCommitFailure},
    domain::{
        session::Session,
        shared_context::{self, SharedContext, Status},
        shared_import::Import,
    },
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

impl Engine {
    pub(super) async fn import_shared_context(&mut self, value: &Value) -> Result<Value> {
        let mut input = Import::parse(value)?;
        ensure!(
            input.workspace.identity() == self.workspace
                && input.workspace.workspace_path == self.workspace_path,
            "Workspace identity mismatch"
        );
        let id = input.session_id.take().unwrap_or_else(|| self.clock.id());
        let history = &mut input.imported_history;
        history.provenance.validate(&id, &history.markdown)?;
        if !self.sessions.contains_key(&id)
            && self
                .store
                .load_session(&self.workspace, &id)
                .await?
                .is_some()
        {
            self.ensure_session(&id).await?;
        }
        if let Some(session) = self.sessions.get(&id) {
            let prior = session
                .shared_context
                .as_ref()
                .context("Session already exists without this shared context")?;
            let mut original = serde_json::to_value(&prior.provenance)?;
            let mut retried = serde_json::to_value(&history.provenance)?;
            original.as_object_mut().unwrap().remove("status");
            retried.as_object_mut().unwrap().remove("status");
            ensure!(
                original == retried,
                "Shared context import conflicts with existing history"
            );
            return self.read_session(&json!({"sessionId":id}));
        }
        let config = if input.model.is_some() {
            self.select(
                &json!({"modelSelection":input.model,"thought":input.thought_level}),
                None,
            )?
        } else {
            self.config.clone().unwrap_or(ModelIdentity {
                provider_id: String::new(),
                model_id: String::new(),
                reasoning_level: String::new(),
            })
        };
        let mut session = Session::new(
            id.clone(),
            self.workspace.clone(),
            config.provider_id,
            config.model_id,
            config.reasoning_level,
            self.clock.id(),
            history.created_at.unwrap_or_else(|| self.clock.now()),
        );
        session.workspace_path = Some(self.workspace_path.clone());
        session.workspace_directory = Some(self.workspace_path.clone());
        session.parent_id = input.parent_session_id;
        session.trace_id = Some(self.clock.id());
        session.title = history.title.clone();
        session.title_source = "custom".into();
        session.phase = "completedSuccess".into();
        if let Some(mode) = input.mode {
            session.plan_enabled = mode == "plan";
            session.mode = if mode == "plan" || mode == "auto" {
                "build".into()
            } else {
                mode
            };
        }
        let content = self
            .store
            .put_attachment(&[history.markdown.as_bytes().to_vec()], "text/markdown")
            .await?;
        if history.provenance.status == Status::Attached {
            session.append_message(json!({"role":"user","content":history.markdown.trim()}));
        }
        // 新导入没有仍存活的 queue owner；不能保留一个永远无法释放的 reserved。
        if history.provenance.status == Status::Reserved {
            history.provenance.status = Status::Pending;
        }
        session.shared_context = Some(SharedContext {
            provenance: history.provenance.clone(),
            content,
            source_id: None,
            attached_message_id: None,
        });
        if let Some(servers) = &input.mcp_servers {
            self.tools.configure_mcp(&id, &json!(servers)).await?;
        }
        self.store
            .commit(&self.workspace, Some(&mut session), None)
            .await
            .context(StorageCommitFailure)?;
        self.sessions.insert(id.clone(), session);
        self.publish(&id, vec![])?;
        self.read_session(&json!({"sessionId":id}))
    }

    pub(super) async fn shared_input(
        &self,
        id: &str,
        p: &Value,
        source: Option<&str>,
    ) -> Result<Option<String>> {
        let Some(reference) = shared_context::reference(p)? else {
            return Ok(None);
        };
        let context = self.sessions[id]
            .shared_context
            .as_ref()
            .context("fault.command.sharedContextNotAttachable")?;
        context.check(&reference, source)?;
        ensure!(
            context.content.total_bytes <= shared_context::MAX_BYTES as u64,
            "Shared context exceeds limit"
        );
        let bytes = self
            .store
            .read_attachment(&context.content, 0, shared_context::MAX_BYTES)
            .await?;
        let text = String::from_utf8(bytes)?;
        context.provenance.check_content(&text)?;
        Ok(Some(text.trim().into()))
    }
}

pub(super) fn attach(
    session: &mut Session,
    p: &Value,
    text: Option<String>,
    source: Option<&str>,
    input_id: &str,
) -> Result<Option<Value>> {
    let Some(reference) = shared_context::reference(p)? else {
        return Ok(None);
    };
    let context = session
        .shared_context
        .as_mut()
        .context("fault.command.sharedContextNotAttachable")?;
    context.check(&reference, source)?;
    let text = text.context("Shared context bytes were not prepared")?;
    context.provenance.status = Status::Attached;
    context.source_id = None;
    context.attached_message_id = Some(input_id.into());
    let message = json!({"role":"user","content":text});
    session.append_message(message.clone());
    Ok(Some(message))
}
pub(super) fn reserve(session: &mut Session, p: &Value, source: &str) -> Result<()> {
    let Some(reference) = shared_context::reference(p)? else {
        return Ok(());
    };
    let context = session
        .shared_context
        .as_mut()
        .context("fault.command.sharedContextNotAttachable")?;
    context.check(&reference, None)?;
    context.provenance.status = Status::Reserved;
    context.source_id = Some(source.into());
    Ok(())
}
pub(super) fn release(session: &mut Session, item: &Value) {
    if let Some(context) = &mut session.shared_context {
        context.release(item["queueItemId"].as_str());
    }
}
