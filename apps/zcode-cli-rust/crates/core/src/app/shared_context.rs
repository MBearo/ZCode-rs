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
        session.phase = crate::domain::execution::Phase::CompletedSuccess;
        if let Some(mode) = input.mode {
            session.plan_enabled = mode == "plan";
            // 导入的 plan/auto 会话降为 build，不能静默提升为可执行模式。
            session.mode = match crate::domain::execution::Mode::parse(&mode) {
                Some(crate::domain::execution::Mode::Auto) | None => {
                    crate::domain::execution::Mode::Build
                }
                Some(mode) => mode,
            };
        }
        // Node：分享上下文作为 model-only 的上下文消息存进会话，不另存产物。
        let journaling = self.journaling();
        let (content, markdown) = if journaling {
            (Default::default(), Some(history.markdown.clone()))
        } else {
            let (_, content) = self
                .store
                .put_attachment(
                    &id,
                    "shared-context-import",
                    &[history.markdown.as_bytes().to_vec()],
                    "text/markdown",
                )
                .await?;
            (content, None)
        };
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
            markdown,
            source_id: None,
            attached_message_id: None,
        });
        if journaling {
            let mut provenance = serde_json::to_value(&history.provenance)?;
            if provenance["shareUrl"].is_null() {
                provenance.as_object_mut().unwrap().remove("shareUrl");
            }
            session.node_shared_import(
                self.clock.now(),
                crate::domain::node_journal::shared::Import {
                    markdown: &history.markdown,
                    provenance,
                    version: env!("CARGO_PKG_VERSION"),
                },
            );
        }
        if let Some(servers) = &input.mcp_servers {
            self.tools.configure_mcp(&id, &json!(servers)).await?;
        }
        self.store
            .commit_receipt(&self.workspace, Some(&mut session), None)
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
        let text = match &context.markdown {
            Some(markdown) => markdown.clone(),
            None => {
                ensure!(
                    context.content.total_bytes <= shared_context::MAX_BYTES as u64,
                    "Shared context exceeds limit"
                );
                let bytes = self
                    .store
                    .read_attachment(&context.content, 0, shared_context::MAX_BYTES)
                    .await?;
                String::from_utf8(bytes)?
            }
        };
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
    // Node：排队准入即把分享上下文预留给该输入（pending → reserved）。
    let now = session.updated_at;
    session.node_shared_transition(now, &reference, (&["pending"], "reserved"), Some(source));
    Ok(())
}
pub(super) fn release(session: &mut Session, item: &Value) {
    let Some(context) = &mut session.shared_context else {
        return;
    };
    let reserved = context.provenance.status == Status::Reserved;
    context.release(item["queueItemId"].as_str());
    if reserved && context.provenance.status == Status::Pending {
        // Node cancelInputCommand：取消的输入把预留还回 pending。
        let reference = context.provenance.context_id.clone().unwrap_or_default();
        let now = session.updated_at;
        let source = item["queueItemId"].as_str();
        session.node_shared_transition(now, &reference, (&["reserved"], "pending"), source);
    }
}
