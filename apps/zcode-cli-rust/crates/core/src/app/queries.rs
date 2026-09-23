use super::Engine;
use crate::contract::{Method, RuntimeError};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
impl Engine {
    fn execution_capabilities(&self) -> Value {
        json!({"permissionModes":["yolo"],"independentPlanState":false})
    }

    pub(super) fn workspace_config(&self) -> Value {
        let options = self.catalog();
        json!({"executionCapabilities":self.execution_capabilities(),"configOptions":if options.is_empty(){vec![]}else{vec![json!({"id":"model","name":"Model","type":"select","currentValue":self.config.as_ref().map(|c|c.model_id.as_str()).unwrap_or(""),"options":options})]},"slashCommands":[{"name":"compact","description":"Compact conversation context","source":"builtin"}]})
    }

    pub(super) fn validate_workspace(&self, p: &Value) -> Result<()> {
        if let Some(workspace) = p.get("workspace") {
            let key = workspace["workspaceIdentity"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .or_else(|| workspace["workspacePath"].as_str())
                .context("Workspace required")?;
            if key != self.workspace {
                bail!("Workspace identity mismatch");
            }
        }
        Ok(())
    }
    pub(super) fn query(&mut self, method: Method, p: &Value) -> Result<Value> {
        self.validate_workspace(p)?;
        match method {
            Method::WorkspaceCancelGenerateText => {
                let operation = p["operationId"].as_str().context("Operation id required")?;
                let ids = self
                    .auxiliary
                    .iter()
                    .filter(|(_, j)| j.operation.as_deref() == Some(operation))
                    .map(|(id, _)| id.clone())
                    .collect::<Vec<_>>();
                for id in &ids {
                    self.auxiliary[id].cancel.cancel();
                    self.cancel_auth(id);
                }
                Ok(json!({"operationId":operation,"cancelled":!ids.is_empty()}))
            }
            Method::RuntimeCapabilities => Ok(
                json!({"workspaceExecutionCapabilities":true,"independentPlanState":false,"accountProviderConfig":self.registry.is_some()}),
            ),
            Method::ProcessChildProcesses => Ok(json!({"processes":[]})),
            Method::WorkspaceReadPresentation => {
                // 旧 App 使用 strict schema；未协商的客户端不能收到新增字段。
                let mut presentation = json!({"workspace":p["workspace"],"mode":"yolo","slashCommands":[{"name":"compact","description":"Compact conversation context","source":"builtin"}]});
                if p["includeExecutionCapabilities"] == true {
                    presentation["executionCapabilities"] = self.execution_capabilities();
                }
                Ok(presentation)
            }
            Method::ConversationRowsRange | Method::ConversationPlans => {
                let session = self
                    .sessions
                    .get(p["sessionId"].as_str().context("Session id required")?)
                    .context("Session unavailable")?;
                if method == Method::ConversationPlans {
                    let plans = session
                        .rows
                        .iter()
                        .rev()
                        .filter(|r| {
                            r["kind"] == "toolCall"
                                && r["toolName"] == "ExitPlanMode"
                                && matches!(
                                    r["status"].as_str(),
                                    Some("success" | "error" | "cancelled")
                                )
                                && serde_json::from_str::<Value>(
                                    r["inputText"].as_str().unwrap_or(""),
                                )
                                .ok()
                                .is_some_and(|v| {
                                    v["plan"].as_str().is_some_and(|p| !p.trim().is_empty())
                                })
                        })
                        .collect::<Vec<_>>();
                    return Ok(
                        json!({"plans":plans,"atSeq":session.seq,"atLogEpoch":session.epoch}),
                    );
                }
                let limit = p["limit"]
                    .as_u64()
                    .filter(|n| *n > 0 && *n <= 200)
                    .ok_or_else(|| RuntimeError::invalid_params("limit: expected 1..=200"))?
                    as usize;
                let before = p["beforeRowId"].as_u64().unwrap_or(u64::MAX);
                let (rows, has_more) =
                    crate::domain::row_page::page(&session.rows, before, limit, 900 * 1024)?;
                Ok(
                    json!({"rows":rows,"atSeq":session.seq,"atRevision":session.revision,"atLogEpoch":session.epoch,"hasMore":has_more}),
                )
            }
            // 路由保证只把查询类方法交到这里；其余方法到达此处说明分派表缺项。
            method => Err(RuntimeError::MethodNotFound(method.as_str().into()).into()),
        }
    }
}
