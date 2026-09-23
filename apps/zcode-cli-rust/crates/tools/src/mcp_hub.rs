use super::{
    mcp_config::{self, Server},
    mcp_connection::Connection,
};
use crate::contract::{ProcessCleanupFailure, ToolOutput};
use anyhow::{Context, Result, ensure};
use futures_util::{StreamExt, stream};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{Arc, RwLock},
};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct Binding {
    name: String,
    original: String,
    key: String,
    safe: bool,
    definition: Value,
    connection: Arc<Connection>,
}
#[derive(Default)]
struct State {
    borrowed: BTreeSet<String>,
    overrides: BTreeMap<String, Value>,
    connections: BTreeMap<String, Arc<Connection>>,
    bindings: BTreeMap<String, Vec<Binding>>,
    statuses: BTreeMap<String, Value>,
}
pub(super) struct Hub {
    cwd: PathBuf,
    http: std::sync::OnceLock<reqwest_mcp::Client>,
    state: RwLock<State>,
    gate: tokio::sync::Mutex<()>,
    stop: CancellationToken,
}
impl Hub {
    pub fn inherit(&self, parent: &str, child: &str) {
        let mut state = self.state.write().unwrap();
        let bindings = state.bindings.get(parent).cloned().unwrap_or_default();
        state.bindings.insert(child.into(), bindings);
        state.borrowed.insert(child.into());
    }
    pub fn new(cwd: PathBuf) -> Self {
        // MCP 的 rustls-no-provider 不自动选择算法；与现有模型 client 统一使用 ring。
        let _ = rustls::crypto::ring::default_provider().install_default();
        Self {
            cwd,
            http: Default::default(),
            state: Default::default(),
            gate: Default::default(),
            stop: CancellationToken::new(),
        }
    }
    pub fn configure(&self, session: &str, servers: &Value) -> Result<()> {
        mcp_config::explicit(servers, &self.cwd)?;
        self.state
            .write()
            .unwrap()
            .overrides
            .insert(session.into(), servers.clone());
        Ok(())
    }
    pub fn safe(&self, session: &str, name: &str) -> bool {
        self.state
            .read()
            .unwrap()
            .bindings
            .get(session)
            .and_then(|b| b.iter().find(|b| b.name == name))
            .is_some_and(|b| b.safe)
    }
    pub async fn definitions(
        &self,
        session: &str,
        cancel: &CancellationToken,
    ) -> Result<Vec<Value>> {
        let overrides = self.state.read().unwrap().overrides.get(session).cloned();
        let borrowed = self.state.read().unwrap().borrowed.contains(session);
        if !borrowed {
            self.prepare(session, overrides.as_ref(), false, cancel)
                .await?;
        }
        Ok(self
            .state
            .read()
            .unwrap()
            .bindings
            .get(session)
            .into_iter()
            .flatten()
            .map(|b| b.definition.clone())
            .collect())
    }
    pub async fn list(&self, p: &Value, cancel: &CancellationToken) -> Result<Value> {
        if p["mode"] == "status" {
            return Ok(json!({"statuses":self.state.read().unwrap().statuses}));
        }
        ensure!(
            p.get("mode").is_none_or(|v| v == "connect"),
            "Invalid MCP list mode"
        );
        self.prepare("mcp-status", p.get("mcpServers"), true, cancel)
            .await?;
        Ok(json!({"statuses":self.state.read().unwrap().statuses}))
    }
    async fn prepare(
        &self,
        session: &str,
        overrides: Option<&Value>,
        refresh: bool,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let _gate = tokio::select! {biased;_=cancel.cancelled()=>anyhow::bail!("Cancelled"),_=self.stop.cancelled()=>anyhow::bail!("MCP stopped"),gate=self.gate.lock()=>gate};
        let request_cancel = self.stop.child_token();
        let relay_cancel = request_cancel.clone();
        let caller = cancel.clone();
        let relay = tokio::spawn(async move {
            caller.cancelled().await;
            relay_cancel.cancel();
        });
        let result = self
            .prepare_inner(session, overrides, refresh, &request_cancel)
            .await;
        relay.abort();
        result
    }
    async fn prepare_inner(
        &self,
        session: &str,
        overrides: Option<&Value>,
        refresh: bool,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let configs = tokio::select! {biased;_=cancel.cancelled()=>anyhow::bail!("Cancelled"),result=mcp_config::configured(&self.cwd,overrides,cancel)=>result?};
        let mut bindings = vec![];
        let mut statuses = BTreeMap::new();
        let mut names = BTreeSet::new();
        let mut futures = stream::iter(configs)
            .map(|server| async move {
                let key = server.key(session);
                let previous = self.state.read().unwrap().connections.get(&key).cloned();
                let result = if !server.enabled {
                    Ok(None)
                } else if server.invalid {
                    Err(anyhow::anyhow!("config_invalid"))
                } else if let Some(previous) =
                    previous.filter(|p| !p.peer.is_transport_closed() && !refresh)
                {
                    Ok(Some(previous))
                } else {
                    let http = if server.transport == "stdio" {
                        None
                    } else {
                        Some(
                            self.http
                                .get_or_init(|| {
                                    reqwest_mcp::Client::builder()
                                        .redirect(reqwest_mcp::redirect::Policy::none())
                                        .connect_timeout(std::time::Duration::from_secs(15))
                                        .build()
                                        .expect("MCP HTTP client")
                                })
                                .clone(),
                        )
                    };
                    Connection::open(&server, http, cancel)
                        .await
                        .map(|c| Some(Arc::new(c)))
                };
                (server, key, result)
            })
            .buffered(4);
        let mut cleanup_failure = None;
        while let Some((server, key, result)) = futures.next().await {
            match result {
                Ok(None) => {
                    statuses.insert(
                        server.name.clone(),
                        mcp_config::status(&server, "disabled", 0, None),
                    );
                }
                Ok(Some(connection)) => {
                    let discovered = bind(&server, &key, &connection, &mut names);
                    match discovered {
                        Ok(entries) => {
                            let mut status =
                                mcp_config::status(&server, "connected", entries.len(), None);
                            status["protocolEra"] = if connection.modern {
                                "modern"
                            } else {
                                "legacy"
                            }
                            .into();
                            statuses.insert(server.name, status);
                            let old = self
                                .state
                                .write()
                                .unwrap()
                                .connections
                                .insert(key, connection.clone());
                            if let Some(old) = old
                                && !Arc::ptr_eq(&old, &connection)
                                && let Err(error) = old.close().await
                            {
                                cleanup_failure = Some(error);
                            }
                            bindings.extend(entries);
                        }
                        Err(_) => {
                            if let Err(error) = connection.close().await {
                                cleanup_failure = Some(error);
                            }
                            statuses.insert(
                                server.name.clone(),
                                mcp_config::status(&server, "failed", 0, Some("tool_list_failed")),
                            );
                        }
                    }
                }
                Err(error) => {
                    if error.is::<ProcessCleanupFailure>() {
                        cleanup_failure = Some(error);
                        continue;
                    }
                    let reason = error.to_string();
                    let kind = match reason.as_str() {
                        "config_invalid" => "config_invalid",
                        "not_authenticated" => "not_authenticated",
                        "connection_timeout" => "connection_timeout",
                        "process_start_failed" => "process_start_failed",
                        "tool_list_failed" => "tool_list_failed",
                        _ => "protocol_negotiation_failed",
                    };
                    statuses.insert(
                        server.name.clone(),
                        mcp_config::status(&server, "failed", 0, Some(kind)),
                    );
                }
            }
        }
        {
            let mut state = self.state.write().unwrap();
            state.bindings.insert(session.into(), bindings);
            state.statuses = statuses;
        }
        self.prune().await?;
        if let Some(error) = cleanup_failure {
            return Err(error);
        }
        super::tools::check_cancel(cancel)
    }
    pub async fn call(
        &self,
        session: &str,
        name: &str,
        args: &Value,
        cancel: &CancellationToken,
    ) -> Result<ToolOutput> {
        let binding = self
            .state
            .read()
            .unwrap()
            .bindings
            .get(session)
            .and_then(|bs| bs.iter().find(|b| b.name == name))
            .cloned()
            .context("MCP tool unavailable in this session")?;
        binding
            .connection
            .call(&binding.original, args, cancel)
            .await
    }
    pub async fn close_session(&self, session: &str, forget: bool) -> Result<()> {
        let _gate = self.gate.lock().await;
        {
            let mut state = self.state.write().unwrap();
            state.bindings.remove(session);
            state.borrowed.remove(session);
            if forget {
                state.overrides.remove(session);
            }
        }
        self.prune().await
    }
    async fn prune(&self) -> Result<()> {
        let removed = {
            let mut state = self.state.write().unwrap();
            let used = state
                .bindings
                .values()
                .flatten()
                .map(|b| b.key.clone())
                .collect::<BTreeSet<_>>();
            let keys = state
                .connections
                .keys()
                .filter(|k| !used.contains(*k))
                .cloned()
                .collect::<Vec<_>>();
            keys.into_iter()
                .filter_map(|k| state.connections.remove(&k))
                .collect::<Vec<_>>()
        };
        for connection in removed {
            connection.close().await?;
        }
        Ok(())
    }
    pub async fn shutdown(&self) -> Result<()> {
        self.stop.cancel();
        let _gate = self.gate.lock().await;
        {
            let mut state = self.state.write().unwrap();
            state.bindings.clear();
            state.overrides.clear();
        }
        self.prune().await
    }
}
fn bind(
    server: &Server,
    key: &str,
    connection: &Arc<Connection>,
    names: &mut BTreeSet<String>,
) -> Result<Vec<Binding>> {
    let mut bindings = vec![];
    for tool in &connection.tools {
        let original = tool["name"]
            .as_str()
            .filter(|s| !s.is_empty())
            .context("MCP tool name required")?;
        let name = mcp_config::tool_name(&server.name, original);
        ensure!(names.insert(name.clone()), "MCP tool namespace collision");
        ensure!(tool["inputSchema"].is_object(), "MCP tool schema required");
        let definition = json!({"type":"function","function":{"name":name,"description":tool["description"].as_str().unwrap_or(""),"parameters":tool["inputSchema"]}});
        ensure!(
            definition.to_string().len() <= 256 * 1024,
            "MCP schema exceeds size limit"
        );
        bindings.push(Binding {
            name,
            original: original.into(),
            key: key.into(),
            safe: tool["annotations"]["readOnlyHint"] == true
                && tool["annotations"]["destructiveHint"] == false,
            definition,
            connection: connection.clone(),
        });
    }
    Ok(bindings)
}
