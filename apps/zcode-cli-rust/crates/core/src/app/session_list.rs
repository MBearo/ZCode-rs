use super::Engine;
use crate::domain::session_listing::{ListParams, MAX_LIST_BYTES};
use anyhow::{Result, ensure};
use serde_json::{Value, json};

impl Engine {
    pub(super) async fn list_sessions(&self, p: &Value) -> Result<Value> {
        let params = ListParams::parse(p)?;
        let records = self
            .store
            .list_sessions(&params, (&self.workspace, &self.workspace_path))
            .await?;
        let sessions = records
            .iter()
            .map(|record| record.projection(params.workspace.as_ref()))
            .collect::<Vec<_>>();
        let result = json!({"sessions":sessions});
        ensure!(
            serde_json::to_vec(&result)?.len() <= MAX_LIST_BYTES,
            "Session list exceeds frame budget; use a smaller limit or sessionIds batch"
        );
        Ok(result)
    }
}
