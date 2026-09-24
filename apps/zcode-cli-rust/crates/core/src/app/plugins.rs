//! `plugins/*` management requests (spec rust-m10-plugins): the tools port
//! answers them in the background like `mcp/list`, cancellable by the actor.
use super::{Engine, auxiliary::Auxiliary, engine::Call};
use crate::contract::{Event, EventSink, RuntimeError};
use anyhow::{Result, ensure};
use tokio_util::sync::CancellationToken;

impl Engine {
    pub(super) fn start_plugin_request(&mut self, request: &Call) -> Result<()> {
        self.validate_workspace(&request.params)?;
        ensure!(self.auxiliary.len() < 16, "Too many auxiliary requests");
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
        let (method, params) = (request.method, request.params.clone());
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
