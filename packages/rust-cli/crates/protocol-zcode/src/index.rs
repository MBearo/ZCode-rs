//! 会话列表（`sessions-index/<workspaceId>`）与工作区配置（`workspace-config/<workspaceId>`）两种主题的发布者。
//!
//! 会话列表的快照 = 存储里这个工作区的会话 + 本进程打开着的会话的实时行；之后每次实时行变了发一个 `session.upserted`。
//! 工作区配置在 P1 只有快照（配置项与斜杠命令为空，斜杠命令由 `workspace/readPresentation` 给）。

use std::collections::HashMap;
use std::sync::Arc;

use rust_cli_protocol_core::delivery::TopicLog;
use serde_json::{Value, json};
use std::sync::{Mutex, MutexGuard, PoisonError};

use rust_cli_protocol_core::serial::Serial;

use crate::context::Context;
use crate::projection::state::Phase;
use crate::wire::Purpose;

/// 会话列表的行里最后一条回复的预览最多这么多个字符。
const REPLY_PREVIEW_CHARS: usize = 200;

pub const INDEX_PREFIX: &str = "sessions-index/";
pub const CONFIG_PREFIX: &str = "workspace-config/";

/// 会话列表的一行（`sessionSummarySchema`）。
pub struct Summary<'a> {
    pub session: &'a str,
    pub workspace: &'a str,
    pub title: &'a str,
    pub phase: Phase,
    pub created_at: u64,
    pub activity_at: u64,
    pub last_reply: Option<&'a str>,
    pub permissions: usize,
}

impl Summary<'_> {
    pub fn to_value(&self) -> Value {
        let mut row = json!({
            "sessionId": self.session,
            "workspaceId": self.workspace,
            "title": self.title,
            "titleSource": if self.title.is_empty() { "default" } else { "generated" },
            "phase": self.phase.name(),
            "sessionEnded": self.phase.ended(),
            "hasBackgroundWork": false,
            "lastActivityAt": self.activity_at,
            "createdAt": self.created_at,
        });
        let outcome = match self.phase {
            Phase::CompletedSuccess => Some("completed"),
            Phase::CompletedInterrupted => Some("interrupted"),
            Phase::Error => Some("error"),
            Phase::Draft | Phase::Running => None,
        };
        if let Some(outcome) = outcome {
            row["lastOutcome"] = json!(outcome);
        }
        if let Some(reply) = self.last_reply.filter(|reply| !reply.is_empty()) {
            let preview: String = reply.chars().take(REPLY_PREVIEW_CHARS).collect();
            row["lastAssistantPreview"] = json!(preview);
        }
        if self.permissions > 0 {
            row["pendingInteractionSummary"] =
                json!({"permissionCount": self.permissions, "userInputCount": 0});
        }
        row
    }
}

struct Published {
    log: TopicLog<Value>,
    /// 实时行（会话 ID → 最新的行）。
    rows: HashMap<String, Value>,
}

/// 一个要写出的逻辑帧（锁里算好，锁外写）。
struct Outgoing {
    topic: String,
    subscription: String,
    range: (u64, u64),
    payload: Value,
    purpose: Purpose,
}

/// 两种工作区级主题的发布者。登记与去掉订阅直接改表；发帧的工作（首帧、实时行）经单写者按顺序做。
pub struct Topics {
    context: Arc<Context>,
    published: Mutex<HashMap<String, Published>>,
    serial: Serial,
}

impl Topics {
    pub fn new(context: Arc<Context>, spawner: &rust_cli_base::Spawner) -> Self {
        Self {
            context,
            published: Mutex::new(HashMap::new()),
            serial: Serial::new(spawner, "index-topics"),
        }
    }

    fn table(&self) -> MutexGuard<'_, HashMap<String, Published>> {
        self.published
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn entry<'a>(
        context: &Context,
        table: &'a mut HashMap<String, Published>,
        topic: &str,
    ) -> &'a mut Published {
        table.entry(topic.to_owned()).or_insert_with(|| Published {
            log: TopicLog::new(context.fresh("epoch")),
            rows: HashMap::new(),
        })
    }

    async fn write(context: &Context, outgoing: Vec<Outgoing>) {
        for frame in outgoing {
            context
                .frame(
                    &frame.topic,
                    &frame.subscription,
                    frame.range,
                    frame.payload,
                    frame.purpose,
                )
                .await;
        }
    }

    /// 登记订阅（还没有首帧），返回主题的代际标识。
    pub fn subscribe(&self, topic: &str, subscription: &str, connection: &str) -> String {
        let mut table = self.table();
        let entry = Self::entry(&self.context, &mut table, topic);
        if let Some(replaced) = entry.log.subscribe(subscription, connection) {
            self.context.forget(&replaced);
        }
        entry.log.epoch().to_owned()
    }

    /// 发首帧（快照，经单写者）。`stored` 是存储里的会话行（会话列表；工作区配置为空），实时行覆盖同一个会话的存储行。
    pub fn start(
        self: &Arc<Self>,
        topic: String,
        subscription: String,
        stored: Vec<Value>,
        purpose: Purpose,
    ) {
        let topics = self.clone();
        self.serial.run(Box::pin(async move {
            let outgoing = topics.snapshot(&topic, &subscription, stored, purpose);
            Self::write(&topics.context, outgoing.into_iter().collect()).await;
        }));
    }

    fn snapshot(
        &self,
        topic: &str,
        subscription: &str,
        stored: Vec<Value>,
        purpose: Purpose,
    ) -> Option<Outgoing> {
        let mut table = self.table();
        let entry = table.get_mut(topic)?;
        let seq = entry.log.start(subscription)?;
        let workspace = topic
            .strip_prefix(INDEX_PREFIX)
            .or_else(|| topic.strip_prefix(CONFIG_PREFIX))
            .unwrap_or_default();
        let snapshot = if topic.starts_with(INDEX_PREFIX) {
            let mut sessions: Vec<Value> = stored
                .into_iter()
                .filter(|row| {
                    row["sessionId"]
                        .as_str()
                        .is_none_or(|id| !entry.rows.contains_key(id))
                })
                .collect();
            sessions.extend(entry.rows.values().cloned());
            json!({"protocolVersion": 1, "workspaceId": workspace, "logEpoch": entry.log.epoch(),
                "sessions": sessions})
        } else {
            json!({"protocolVersion": 1, "workspaceId": workspace, "logEpoch": entry.log.epoch(),
                "config": {"configOptions": [], "slashCommands": []}})
        };
        Some(Outgoing {
            topic: topic.to_owned(),
            subscription: subscription.to_owned(),
            range: (0, seq),
            payload: json!({"kind": "snapshot", "snapshot": snapshot}),
            purpose,
        })
    }

    /// 重新同步：下一次首帧重新取快照。订阅不在时返回 None，否则给代际标识。
    pub fn restart(&self, topic: &str, subscription: &str) -> Option<String> {
        let mut table = self.table();
        let entry = table.get_mut(topic)?;
        entry
            .log
            .restart(subscription)
            .then(|| entry.log.epoch().to_owned())
    }

    pub fn unsubscribe(&self, topic: &str, subscription: &str) -> bool {
        let removed = self
            .table()
            .get_mut(topic)
            .is_some_and(|entry| entry.log.unsubscribe(subscription));
        if removed {
            self.context.forget(subscription);
        }
        removed
    }

    pub fn drop_connection(&self, connection: &str) {
        let mut table = self.table();
        for entry in table.values_mut() {
            for gone in entry.log.drop_connection(connection) {
                self.context.forget(&gone);
            }
        }
    }

    /// 会话列表的一行变了（经单写者）：记下并发给已经开始的订阅者。
    pub fn upsert(self: &Arc<Self>, workspace: &str, row: Value) {
        let topics = self.clone();
        let topic = format!("{INDEX_PREFIX}{workspace}");
        self.serial.run(Box::pin(async move {
            let outgoing = topics.changed(&topic, row);
            Self::write(&topics.context, outgoing).await;
        }));
    }

    fn changed(&self, topic: &str, row: Value) -> Vec<Outgoing> {
        let Some(session) = row["sessionId"].as_str().map(str::to_owned) else {
            return Vec::new();
        };
        let mut table = self.table();
        let entry = Self::entry(&self.context, &mut table, topic);
        if entry.rows.get(&session) == Some(&row) {
            return Vec::new();
        }
        entry.rows.insert(session, row.clone());
        entry
            .log
            .record(vec![json!({"op": "session.upserted", "session": row})]);
        entry
            .log
            .flush()
            .into_iter()
            .map(|batch| Outgoing {
                topic: topic.to_owned(),
                subscription: batch.subscription,
                range: (batch.from, batch.to),
                payload: json!({"kind": "deltas", "deltas": batch.deltas}),
                purpose: Purpose::Online,
            })
            .collect()
    }
}
