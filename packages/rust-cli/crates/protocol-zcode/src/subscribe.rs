//! 订阅（docs/specs/rust-cli/app-protocol.md §6）：`v4/conversation/subscribe` / `unsubscribe` / `resync`、
//! `v4/connection/flow` 与 `v4/sessionsIndex/reconcile`。P1 只有 desktop-continuous：回复只有 ACK（mode 一律 snapshot），
//! 首帧在回复之后发；resync 一律回快照。

use std::sync::Arc;

use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::store::SessionFilter;
use rust_cli_protocol_core::connection::Reply;
use rust_cli_protocol_core::rpc::RpcError;
use serde_json::{Value, json};

use crate::index::{CONFIG_PREFIX, INDEX_PREFIX, Summary};
use crate::live::{CONVERSATION_PREFIX, Live};
use crate::projection::state::Phase;
use crate::server::{Server, kernel_error};
use crate::wire::Purpose;
use rust_cli_protocol_core::sessions::Bridged;

/// 会话列表的快照最多从存储里取这么多个会话。
const STORED_SESSIONS_LIMIT: usize = 200;

/// 存储里一个工作区的会话 → 会话列表的行。
async fn stored_rows(server: &Server, workspace: &str) -> Vec<Value> {
    let filter = SessionFilter {
        workspace: server.workspace(workspace),
        limit: STORED_SESSIONS_LIMIT,
    };
    match server.kernel.workspace_sessions(filter).await {
        Ok(sessions) => sessions
            .into_iter()
            .map(|session| {
                let summary = &session.summary;
                Summary {
                    session: summary.id.as_str(),
                    workspace,
                    title: &summary.title,
                    phase: Phase::CompletedSuccess,
                    created_at: summary.updated_at_ms,
                    activity_at: summary.updated_at_ms,
                    last_reply: session.last_reply.as_deref(),
                    permissions: 0,
                }
                .to_value()
            })
            .collect(),
        Err(error) => {
            tracing::warn!(event = "protocol.index.read_failed", error = %error, "listing the workspace sessions failed");
            Vec::new()
        }
    }
}

fn ack(subscription: &str, epoch: &str) -> Value {
    json!({"ack": {"subscriptionId": subscription, "mode": "snapshot", "logEpoch": epoch}})
}

pub async fn subscribe(server: &Server, params: &Value) -> Result<Reply, RpcError> {
    let topic = params["topic"].as_str().unwrap_or_default().to_owned();
    let connection = params["connectionId"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    if let Some(session) = topic.strip_prefix(CONVERSATION_PREFIX) {
        let live = server
            .live(&SessionId::new(session))
            .await
            .map_err(|error| kernel_error(&error))?;
        let subscription = server.context.fresh("sub");
        let epoch = live.sink.subscribe(&subscription, &connection);
        return Ok(Reply::then(
            ack(&subscription, &epoch),
            start_on_pump(live, subscription, Purpose::Initial),
        ));
    }
    let index = topic.strip_prefix(INDEX_PREFIX);
    if index.is_none() && !topic.starts_with(CONFIG_PREFIX) {
        return Err(RpcError::invalid_params(format!("unknown topic {topic}")));
    }
    let stored = match index {
        Some(workspace) => stored_rows(server, workspace).await,
        None => Vec::new(),
    };
    let subscription = server
        .context
        .fresh(if index.is_some() { "six" } else { "wcs" });
    let epoch = server.topics.subscribe(&topic, &subscription, &connection);
    let topics = server.topics.clone();
    let id = subscription.clone();
    Ok(Reply::then(
        ack(&subscription, &epoch),
        Box::new(move || {
            topics.start(topic, id, stored, Purpose::Initial);
            Box::pin(async {})
        }),
    ))
}

pub async fn unsubscribe(server: &Server, params: &Value) -> Result<Reply, RpcError> {
    let topic = params["topic"].as_str().unwrap_or_default();
    let subscription = params["subscriptionId"].as_str().unwrap_or_default();
    if let Some(session) = topic.strip_prefix(CONVERSATION_PREFIX) {
        if let Some(live) = server.bridge.get(&SessionId::new(session)) {
            live.sink.unsubscribe(subscription);
        }
    } else {
        server.topics.unsubscribe(topic, subscription);
    }
    Ok(Reply::new(json!({})))
}

/// 重新同步：P1 一律回快照（续传在 P2）。
pub async fn resync(server: &Server, params: &Value) -> Result<Reply, RpcError> {
    let topic = params["topic"].as_str().unwrap_or_default().to_owned();
    let subscription = params["subscriptionId"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let gone = || RpcError::invalid_params(format!("unknown subscription {subscription}"));
    if let Some(session) = topic.strip_prefix(CONVERSATION_PREFIX) {
        let live = server
            .bridge
            .get(&SessionId::new(session))
            .ok_or_else(gone)?;
        let epoch = live.sink.restart(&subscription).ok_or_else(gone)?;
        return Ok(Reply::then(
            ack(&subscription, &epoch),
            start_on_pump(live, subscription, Purpose::Recovery),
        ));
    }
    let epoch = server
        .topics
        .restart(&topic, &subscription)
        .ok_or_else(gone)?;
    let stored = match topic.strip_prefix(INDEX_PREFIX) {
        Some(workspace) => stored_rows(server, workspace).await,
        None => Vec::new(),
    };
    let topics = server.topics.clone();
    let id = subscription.clone();
    Ok(Reply::then(
        ack(&subscription, &epoch),
        Box::new(move || {
            topics.start(topic, id, stored, Purpose::Recovery);
            Box::pin(async {})
        }),
    ))
}

/// 下游背压：P1 只在 closed 时去掉这条连接的订阅（暂停与恢复在 P2）。
pub async fn flow(server: &Server, params: &Value) -> Result<Reply, RpcError> {
    if params["state"] == "closed" {
        let connection = params["connectionId"].as_str().unwrap_or_default();
        server.topics.drop_connection(connection);
        for live in server.bridge.sessions() {
            live.sink.drop_connection(connection);
        }
    }
    Ok(Reply::new(json!({})))
}

/// 会话列表对账：按 sessionIds 回存在与否、标题与时间。
pub async fn reconcile(server: &Server, params: &Value) -> Result<Reply, RpcError> {
    let workspace = params["workspaceId"].as_str().unwrap_or_default();
    let ids: Vec<&str> = params["sessionIds"]
        .as_array()
        .map(|ids| ids.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut sessions = Vec::new();
    for id in ids {
        let session = SessionId::new(id);
        let row = match server.bridge.get(&session) {
            Some(live) => Some(live.sink.summary()),
            None => match server.kernel.session_snapshot(&session).await {
                Ok(view) => Some(
                    Summary {
                        session: id,
                        workspace,
                        title: &view.meta.title,
                        phase: Phase::CompletedSuccess,
                        created_at: view.meta.created_at_ms,
                        activity_at: view.meta.created_at_ms,
                        last_reply: None,
                        permissions: 0,
                    }
                    .to_value(),
                ),
                Err(_) => None,
            },
        };
        sessions.push(match row {
            Some(row) => {
                let mut entry = json!({
                    "sessionId": id,
                    "exists": true,
                    "deleted": false,
                    "title": row["title"],
                    "titleSource": row["titleSource"],
                    "createdAt": row["createdAt"],
                    "updatedAt": row["lastActivityAt"],
                });
                if let Some(outcome) = row.get("lastOutcome") {
                    entry["lastOutcome"] = outcome.clone();
                }
                entry
            }
            None => json!({"sessionId": id, "exists": false, "deleted": false}),
        });
    }
    Ok(Reply::new(json!({"sessions": sessions})))
}

/// 新建的会话马上进会话列表（在会话的泵任务里发）。
pub fn announce(live: &Arc<Bridged<Live>>) {
    let sink = live.sink.clone();
    live.run(Box::pin(async move { sink.publish_summary().await }));
}

/// 回复之后在会话的泵任务里发首帧（排在已经收到的事件之后）。
fn start_on_pump(
    live: Arc<Bridged<Live>>,
    subscription: String,
    purpose: Purpose,
) -> rust_cli_protocol_core::connection::After {
    Box::new(move || {
        let sink = live.sink.clone();
        live.run(Box::pin(
            async move { sink.start(&subscription, purpose).await },
        ));
        Box::pin(async {})
    })
}
