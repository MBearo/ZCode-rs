//! `plugins/*` management requests (spec rust-m10-plugins): the tools port
//! answers them in the background like `mcp/list`, cancellable by the actor.
use super::{Engine, auxiliary::Auxiliary, engine::Call};
use crate::contract::{Event, EventSink, RuntimeError};
use anyhow::{Result, ensure};
use tokio_util::sync::CancellationToken;

impl Engine {
    pub(super) async fn start_plugin_request(&mut self, request: &Call) -> Result<()> {
        self.validate_workspace(&request.params)?;
        ensure!(self.auxiliary.len() < 16, "Too many auxiliary requests");
        let mut params = request.params.clone();
        // 带 sessionId 的引用目录返回该会话冻结的目录（会话不存在即失败，不回退 workspace）。
        if request
            .method
            .as_str()
            .starts_with("plugins/referenceCatalog")
            && let Some(id) = params["sessionId"].as_str().map(str::to_owned)
        {
            self.ensure_session(&id).await?;
            let catalog = self.session_plugin_catalog(&id).await?;
            params["frozenCatalog"] = serde_json::Value::Array(catalog.to_vec());
        }
        let id = format!("plugins:{}", self.clock.id());
        let cancel = CancellationToken::new();
        self.auxiliary.insert(
            id.clone(),
            Auxiliary {
                token: request.token,
                cancel: cancel.clone(),
                operation: None,
            },
        );
        let sink = EventSink {
            session_id: id.clone(),
            run_id: id,
            tx: self.events.clone(),
            origin: crate::contract::RequestOrigin::detached(self.clock.id()),
            request_auth: None,
        };
        let tools = self.tools.clone();
        let method = request.method;
        tokio::spawn(async move {
            let result = tools
                .plugins(method.as_str(), &params, &cancel)
                .await
                .map_err(|error| RuntimeError::Fault {
                    message: format!("{error:#}"),
                    code: None,
                });
            let _ = sink.send(Event::AuxiliaryReply { result }).await;
        });
        Ok(())
    }
}
