use super::{Engine, auxiliary::Auxiliary, engine::Call};
use crate::contract::{Event, EventSink, ModelFailure, ProcessCleanupFailure};
use anyhow::{Result, ensure};
use tokio_util::sync::CancellationToken;

impl Engine {
    pub(super) fn start_mcp_query(&mut self, request: &Call) -> Result<()> {
        self.validate_workspace(&request.params)?;
        ensure!(self.auxiliary.len() < 16, "Too many auxiliary requests");
        let id = format!("mcp-query:{}", self.clock.id());
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
        };
        let tools = self.tools.clone();
        let params = request.params.clone();
        tokio::spawn(async move {
            let result = tools.mcp_list(&params, &cancel).await;
            if result
                .as_ref()
                .is_err_and(|e| e.is::<ProcessCleanupFailure>())
            {
                let _ = sink
                    .send(Event::ToolCleanupFailed(
                        "MCP process cleanup failed".into(),
                    ))
                    .await;
            } else {
                let result =
                    result.map_err(|_| ModelFailure::new("mcp_configuration_failed", false));
                let _ = sink.send(Event::AuxiliaryDone { result }).await;
            }
        });
        Ok(())
    }
}
