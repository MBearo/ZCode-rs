//! 内存里的会话存储。

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;

use super::lock;
use crate::ids::ArtifactId;
use crate::ids::MessageId;
use crate::ids::SessionId;
use crate::message::AttachmentMessage;
use crate::message::Message;
use crate::store::{
    ArtifactPurpose, ArtifactRef, ArtifactStore, CommitReceipt, Fact, FactBatch, ForkReceipt,
    LoadedSession, SessionFilter, SessionFork, SessionKind, SessionMeta, SessionOrder, SessionPage,
    SessionQuery, SessionRecord, SessionStore, SessionSummary, StoreError,
};

/// 重放事实得到活动历史与 checkpoint 清单：`RevertSet` 之后活动历史 = `kept` + 切点之后追加的
/// 消息（docs/specs/rust-cli/session-lifecycle.md §5.3）；`CheckpointsRestored` 去掉撤销过的。
fn replay(facts: &[Fact]) -> (Vec<Message>, Vec<crate::store::WorkspaceCheckpoint>) {
    let mut all: Vec<Message> = Vec::new();
    let mut active: Vec<usize> = Vec::new();
    let mut checkpoints = Vec::new();
    for fact in facts {
        match fact {
            Fact::MessageAppended { message } => {
                all.push(message.clone());
                active.push(all.len() - 1);
            }
            Fact::RevertSet { cursor } => {
                active = cursor
                    .kept
                    .iter()
                    .filter_map(|id| all.iter().position(|m| m.id() == id))
                    .collect();
            }
            Fact::CheckpointCreated { checkpoint } => checkpoints.push(checkpoint.clone()),
            Fact::CheckpointsRestored { checkpoints: gone } => {
                checkpoints.retain(|c: &crate::store::WorkspaceCheckpoint| !gone.contains(&c.id));
            }
            _ => {}
        }
    }
    (
        active.into_iter().map(|i| all[i].clone()).collect(),
        checkpoints,
    )
}

/// 冷读取按 node-store 的形状切分（工作线 F102，docs/specs/rust-cli/compact.md §5.1）：活动历史（最后一个压缩点起，
/// 保留段接在摘要之后）是模型侧的历史；之前的、不在活动历史里、也不是保留段首尾的消息只供显示。返回（只供显示，模型侧）。
fn split_cold(messages: Vec<Message>) -> (Vec<Message>, Vec<Message>) {
    let Some((start, boundary)) = crate::history::last_boundary(&messages) else {
        return (Vec::new(), messages);
    };
    let mut skip: HashSet<MessageId> = HashSet::new();
    if let Some(segment) = &boundary.preserved {
        skip.extend([segment.head.clone(), segment.tail.clone()]);
    }
    let active: Vec<Message> = crate::history::active(&messages)
        .into_iter()
        .map(std::borrow::Cow::into_owned)
        .collect();
    skip.extend(active.iter().map(|message| message.id().clone()));
    let earlier = messages[..start]
        .iter()
        .filter(|message| !skip.contains(message.id()))
        .cloned()
        .collect();
    (earlier, active)
}

/// 内存里的会话存储：记录每次提交的事实，`load` 重放消息、回退游标与 checkpoint。
#[derive(Default)]
pub struct MemoryStore {
    sessions: Mutex<HashMap<SessionId, (SessionMeta, Vec<Fact>)>>,
    sequence: AtomicU64,
    /// 按工作区根记的本机项目规则集（`/permissions`，tui-manage.md §1.1）。
    project_rules: Mutex<HashMap<String, crate::permission::ProjectRules>>,
    /// `activity` 按范围给出的结果（usage.md §5；聚合在 node-store，这里只给前端测试用）。
    activity: Mutex<HashMap<crate::metering::ActivityRange, crate::metering::ActivityStats>>,
    /// 已信任的工作区（workspace-trust.md §2.3）。
    trusted: Mutex<Vec<crate::workspace_trust::TrustKey>>,
    /// 每个批次带的回合锚点，按提交顺序（node-store.md §4，修复线 F6）。
    turns: Mutex<HashMap<SessionId, Vec<Option<crate::store::TurnAnchor>>>>,
    /// 软删除的会话（修复线 F29，node-store.md §5.1）：事实保留，读、列、提交都当作不存在。
    deleted: Mutex<HashSet<SessionId>>,
    /// 测试给的活动时间（`set_updated`）；没有时取创建时间。
    updated: Mutex<HashMap<SessionId, u64>>,
    /// 前端记录（缺口 G2）：按写入顺序，同 key 覆盖。
    records: Mutex<HashMap<SessionId, Vec<crate::store::FrontendRecord>>>,
    /// 冷读取按 node-store 的形状切分压缩过的会话（[`Self::split_at_compaction`]，工作线 F102）。
    split: std::sync::atomic::AtomicBool,
}

impl MemoryStore {
    /// 之后的 `load` 按 node-store 的形状给出压缩过的会话（工作线 F102，compact.md §5.1）：`messages` 从最后一个压缩点开始
    /// （保留段接在摘要之后），之前的放进 `earlier`。缺省不切分（`messages` 是重放出的全部消息）。
    pub fn split_at_compaction(&self) {
        self.split.store(true, Ordering::SeqCst);
    }

    /// 已写下的信任记录（按写入顺序，重复写只留一条）。
    pub fn trusted_keys(&self) -> Vec<crate::workspace_trust::TrustKey> {
        lock(&self.trusted).clone()
    }

    /// 每个成功提交的批次带的回合锚点，按提交顺序。
    pub fn batch_turns(&self, session: &SessionId) -> Vec<Option<crate::store::TurnAnchor>> {
        lock(&self.turns).get(session).cloned().unwrap_or_default()
    }

    pub fn facts(&self, session: &SessionId) -> Vec<Fact> {
        lock(&self.sessions)
            .get(session)
            .map(|(_, facts)| facts.clone())
            .unwrap_or_default()
    }

    /// 会话的活动时间（`query_sessions` 的排序与显示，测试用）。
    pub fn set_updated(&self, session: &SessionId, at_ms: u64) {
        lock(&self.updated).insert(session.clone(), at_ms);
    }

    /// 已删除的会话（修复线 F29）。
    pub fn is_deleted(&self, session: &SessionId) -> bool {
        lock(&self.deleted).contains(session)
    }

    /// 会话现在的标题（最后一次写入；node-store 的会话行）。
    pub fn title(&self, session: &SessionId) -> Option<String> {
        let sessions = lock(&self.sessions);
        let (meta, facts) = sessions.get(session)?;
        Some(last_title(facts).unwrap_or_else(|| meta.title.clone()))
    }

    /// 之后 `activity(range)` 返回的结果。
    pub fn set_activity(
        &self,
        range: crate::metering::ActivityRange,
        stats: crate::metering::ActivityStats,
    ) {
        lock(&self.activity).insert(range, stats);
    }
}

#[async_trait]
impl SessionStore for MemoryStore {
    async fn create(&self, meta: SessionMeta) -> Result<(), StoreError> {
        lock(&self.sessions).insert(meta.id.clone(), (meta, Vec::new()));
        Ok(())
    }

    async fn commit(
        &self,
        session: &SessionId,
        batch: FactBatch,
    ) -> Result<CommitReceipt, StoreError> {
        if self.is_deleted(session) {
            return Err(StoreError::NotFound {
                session: session.clone(),
            });
        }
        let mut sessions = lock(&self.sessions);
        let (meta, facts) = sessions
            .get_mut(session)
            .ok_or_else(|| StoreError::NotFound {
                session: session.clone(),
            })?;
        // “总是允许”与 node-store 一样写进会话所在项目的规则集（追加、去重）。
        for fact in &batch.facts {
            if let Fact::PermissionRulesAdded { behavior, rules } = fact {
                let mut all = lock(&self.project_rules);
                let project = all
                    .entry(meta.workspace.root.as_str().to_owned())
                    .or_default();
                let list = match behavior {
                    crate::permission::RuleBehavior::Allow => &mut project.allow,
                    crate::permission::RuleBehavior::Deny => &mut project.deny,
                    crate::permission::RuleBehavior::Ask => &mut project.ask,
                };
                for rule in rules {
                    if !list.contains(rule) {
                        list.push(rule.clone());
                    }
                }
            }
        }
        facts.extend(batch.facts);
        lock(&self.turns)
            .entry(session.clone())
            .or_default()
            .push(batch.turn);
        Ok(CommitReceipt {
            sequence: self.sequence.fetch_add(1, Ordering::Relaxed) + 1,
        })
    }

    async fn load(&self, session: &SessionId) -> Result<LoadedSession, StoreError> {
        if self.is_deleted(session) {
            return Err(StoreError::NotFound {
                session: session.clone(),
            });
        }
        let sessions = lock(&self.sessions);
        let (meta, facts) = sessions.get(session).ok_or_else(|| StoreError::NotFound {
            session: session.clone(),
        })?;
        let (messages, checkpoints) = replay(facts);
        // 标题与 node-store 的会话行一样取最后一次写入（会话关闭后再打开时 actor 从这里拿标题）。
        let title = last_title(facts);
        let mut meta = meta.clone();
        if let Some(title) = title {
            meta.title = title;
        }
        let (earlier, messages) = if self.split.load(Ordering::SeqCst) {
            split_cold(messages)
        } else {
            (Vec::new(), messages)
        };
        Ok(LoadedSession {
            meta,
            messages,
            earlier,
            model: None,
            mode: None,
            plan: false,
            open_operations: Vec::new(),
            pending_interactions: Vec::new(),
            checkpoints,
            // 最后一个批次之后的当前轮（node-store 按库推导；这里取最后一个带锚点的批次）。
            turn: lock(&self.turns)
                .get(session)
                .and_then(|turns| turns.iter().rev().find_map(Clone::clone)),
            last_turn_failed: last_turn_failed(facts),
        })
    }

    /// 前端记录（缺口 G2）：会话要存在；同 key 覆盖原位置。
    async fn put_record(
        &self,
        session: &SessionId,
        record: &crate::store::FrontendRecord,
    ) -> Result<(), StoreError> {
        if !lock(&self.sessions).contains_key(session) {
            return Err(StoreError::NotFound {
                session: session.clone(),
            });
        }
        let mut records = lock(&self.records);
        let list = records.entry(session.clone()).or_default();
        match list.iter_mut().find(|known| known.key == record.key) {
            Some(known) => *known = record.clone(),
            None => list.push(record.clone()),
        }
        Ok(())
    }

    async fn records(
        &self,
        session: &SessionId,
        kind: &str,
    ) -> Result<Vec<crate::store::FrontendRecord>, StoreError> {
        Ok(lock(&self.records)
            .get(session)
            .map(|list| {
                list.iter()
                    .filter(|record| record.kind == kind)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
    }

    /// `Fact::Entry` 按写入顺序（node-store 的同一口径，cross-session.md §2.2）。
    async fn entries(
        &self,
        session: &SessionId,
        kind: &str,
    ) -> Result<Vec<(String, serde_json::Value)>, StoreError> {
        let sessions = lock(&self.sessions);
        let (_, facts) = sessions.get(session).ok_or_else(|| StoreError::NotFound {
            session: session.clone(),
        })?;
        Ok(facts
            .iter()
            .filter_map(|fact| match fact {
                Fact::Entry {
                    kind: entry,
                    id,
                    data,
                } if entry == kind => Some((id.clone(), data.clone())),
                _ => None,
            })
            .collect())
    }

    /// 会话与以它为祖先的会话里 `ModelCalled{status: completed}` 的耗时之和（node-store 的同一口径，usage.md §4）。
    async fn metering(
        &self,
        session: &SessionId,
    ) -> Result<crate::metering::SessionMetering, StoreError> {
        let sessions = lock(&self.sessions);
        if !sessions.contains_key(session) {
            return Err(StoreError::NotFound {
                session: session.clone(),
            });
        }
        let descends = |start: &SessionId| {
            let mut id = start.clone();
            loop {
                if &id == session {
                    return true;
                }
                match sessions.get(&id).and_then(|(meta, _)| meta.parent.clone()) {
                    Some(parent) => id = parent,
                    None => return false,
                }
            }
        };
        let api_duration_ms = sessions
            .iter()
            .filter(|(id, _)| descends(id))
            .flat_map(|(_, (_, facts))| facts)
            .filter_map(|fact| match fact {
                Fact::ModelCalled { call }
                    if call.status == crate::metering::CallStatus::Completed =>
                {
                    Some(call.completed_at_ms.saturating_sub(call.started_at_ms))
                }
                _ => None,
            })
            .sum();
        Ok(crate::metering::SessionMetering { api_duration_ms })
    }

    /// 会话里 `ModelCalled{source: 主线程或子代理, status: completed}` 的聚合（node-store 的同一口径，usage.md §4.1；
    /// 子代理的请求只在子会话里，F112）：用量缺失时输入与缓存读都算未知；“最近一个”是最后提交的那个。
    async fn request_stats(
        &self,
        session: &SessionId,
    ) -> Result<crate::metering::RequestStats, StoreError> {
        let sessions = lock(&self.sessions);
        let (_, facts) = sessions.get(session).ok_or_else(|| StoreError::NotFound {
            session: session.clone(),
        })?;
        let mut stats = crate::metering::RequestStats::default();
        for fact in facts {
            let Fact::ModelCalled { call } = fact else {
                continue;
            };
            let own_loop = matches!(
                call.source,
                crate::model::QuerySource::MainThread | crate::model::QuerySource::Subagent(_)
            );
            if !own_loop || call.status != crate::metering::CallStatus::Completed {
                continue;
            }
            stats.requests += 1;
            match &call.usage {
                // Node 归一化的输入含缓存读写（usage.md §2.2）。
                Some(usage) => {
                    stats.input_tokens += usage.input_tokens
                        + usage.cache_creation_input_tokens
                        + usage.cache_read_input_tokens;
                    stats.cache_read_tokens += usage.cache_read_input_tokens;
                }
                None => stats.unknown_cache_usage = true,
            }
            stats.last = Some(crate::metering::LastRequest {
                output_tokens: call.usage.map(|usage| usage.output_tokens),
                duration_ms: Some(call.completed_at_ms.saturating_sub(call.started_at_ms)),
                first_token_ms: call
                    .first_token_at_ms
                    .map(|first| first.saturating_sub(call.started_at_ms)),
            });
        }
        Ok(stats)
    }

    async fn activity(
        &self,
        query: crate::metering::ActivityQuery,
    ) -> Result<crate::metering::ActivityStats, StoreError> {
        Ok(lock(&self.activity)
            .get(&query.range)
            .cloned()
            .unwrap_or_else(|| crate::metering::ActivityStats {
                range: Some(query.range),
                today: query.today,
                ..Default::default()
            }))
    }

    /// Node `/fork` 的替身：活动历史到 `through` 为止原样复制（内存里不必换 ID），末尾是一条
    /// `source = fork` 的 attachment（正文是简化的分叉提示）。
    async fn fork(&self, fork: SessionFork) -> Result<ForkReceipt, StoreError> {
        let mut sessions = lock(&self.sessions);
        let (parent, facts) =
            sessions
                .get(&fork.parent)
                .cloned()
                .ok_or_else(|| StoreError::NotFound {
                    session: fork.parent.clone(),
                })?;
        let (messages, _) = replay(&facts);
        let end = messages
            .iter()
            .position(|m| m.id() == &fork.through)
            .ok_or_else(|| StoreError::Storage {
                message: format!(
                    "Checkpoint message not found in session store: {}",
                    fork.through
                ),
            })?
            + 1;
        let title = if parent.title.is_empty() {
            crate::title::UNTITLED
        } else {
            parent.title.as_str()
        };
        let meta = SessionMeta {
            id: fork.child.clone(),
            kind: SessionKind::Fork,
            parent: Some(parent.id.clone()),
            workspace: parent.workspace.clone(),
            title: format!("Fork of {title}"),
            trace: parent.trace.clone(),
            created_at_ms: fork.created_at_ms,
        };
        let notice = Message::Attachment(AttachmentMessage {
            id: MessageId::new(format!("msg_fork_notice_{}", fork.child)),
            source: "fork".to_owned(),
            text: rust_cli_base::reminder::wrap(&format!(
                "This session was forked from {}.",
                parent.id
            )),
            data: serde_json::Value::Null,
        });
        let mut copied = vec![
            Fact::ModelSelected {
                selection: fork.selection,
            },
            Fact::ExecutionChanged {
                state: crate::permission::ExecutionState::from_parts(fork.mode, fork.plan),
            },
        ];
        copied.extend(
            messages[..end]
                .iter()
                .chain(std::iter::once(&notice))
                .map(|message| Fact::MessageAppended {
                    message: message.clone(),
                }),
        );
        sessions.insert(fork.child, (meta, copied));
        Ok(ForkReceipt { copied: end })
    }

    /// node-store 的同一口径（node-store.md §5.1）：按根目录筛、删除的不列、按时间从新到旧（同一时刻 id 大的在前）。
    async fn query_sessions(&self, query: SessionQuery) -> Result<SessionPage, StoreError> {
        let deleted = lock(&self.deleted).clone();
        let updated = lock(&self.updated).clone();
        let mut records: Vec<SessionRecord> = lock(&self.sessions)
            .values()
            .filter(|(meta, _)| !deleted.contains(&meta.id) && meta.kind != SessionKind::Subagent)
            .filter(|(meta, _)| {
                query
                    .root
                    .as_ref()
                    .is_none_or(|root| &meta.workspace.root == root)
            })
            .map(|(meta, facts)| SessionRecord {
                id: meta.id.clone(),
                title: last_title(facts).unwrap_or_else(|| meta.title.clone()),
                root: meta.workspace.root.clone(),
                created_at_ms: meta.created_at_ms,
                updated_at_ms: updated.get(&meta.id).copied().unwrap_or(meta.created_at_ms),
                first_prompt: first_prompt(facts),
            })
            .collect();
        records.sort_by(|a, b| {
            let key = |r: &SessionRecord| match query.order {
                SessionOrder::Updated => r.updated_at_ms,
                SessionOrder::Created => r.created_at_ms,
            };
            key(b)
                .cmp(&key(a))
                .then_with(|| b.id.as_str().cmp(a.id.as_str()))
        });
        let more = query.limit > 0 && records.len() > query.limit;
        if query.limit > 0 {
            records.truncate(query.limit);
        }
        Ok(SessionPage {
            sessions: records,
            more,
        })
    }

    async fn rename(&self, session: &SessionId, title: &str) -> Result<(), StoreError> {
        if self.is_deleted(session) {
            return Err(StoreError::NotFound {
                session: session.clone(),
            });
        }
        let mut sessions = lock(&self.sessions);
        let (_, facts) = sessions
            .get_mut(session)
            .ok_or_else(|| StoreError::NotFound {
                session: session.clone(),
            })?;
        facts.push(Fact::TitleChanged {
            title: title.to_owned(),
        });
        Ok(())
    }

    async fn delete(&self, session: &SessionId) -> Result<(), StoreError> {
        if !lock(&self.sessions).contains_key(session) {
            return Err(StoreError::NotFound {
                session: session.clone(),
            });
        }
        lock(&self.deleted).insert(session.clone());
        Ok(())
    }

    async fn list(&self, _filter: SessionFilter) -> Result<Vec<SessionSummary>, StoreError> {
        let deleted = lock(&self.deleted).clone();
        Ok(lock(&self.sessions)
            .values()
            .filter(|(meta, _)| !deleted.contains(&meta.id))
            .map(|(meta, _)| SessionSummary {
                id: meta.id.clone(),
                title: meta.title.clone(),
                updated_at_ms: meta.created_at_ms,
            })
            .collect())
    }

    async fn project_rules(
        &self,
        workspace: &crate::env::WorkspaceRef,
    ) -> Result<crate::permission::ProjectRules, StoreError> {
        Ok(lock(&self.project_rules)
            .get(workspace.root.as_str())
            .cloned()
            .unwrap_or_default())
    }

    /// 与 node-store 相同的语义：这一类里先删、再加并去重（保留先出现的）。
    async fn change_project_rules(
        &self,
        workspace: &crate::env::WorkspaceRef,
        change: crate::store::ProjectRulesChange,
    ) -> Result<crate::permission::ProjectRules, StoreError> {
        use crate::permission::RuleBehavior;
        let mut all = lock(&self.project_rules);
        let rules = all.entry(workspace.root.as_str().to_owned()).or_default();
        let list = match change.behavior {
            RuleBehavior::Allow => &mut rules.allow,
            RuleBehavior::Deny => &mut rules.deny,
            RuleBehavior::Ask => &mut rules.ask,
        };
        list.retain(|rule| !change.remove.contains(rule));
        for rule in change.add {
            if !list.contains(&rule) {
                list.push(rule);
            }
        }
        Ok(rules.clone())
    }

    async fn trusted_workspace(
        &self,
        candidates: &[crate::workspace_trust::TrustKey],
    ) -> Result<Option<crate::workspace_trust::TrustKey>, StoreError> {
        let trusted = lock(&self.trusted);
        Ok(candidates.iter().find(|key| trusted.contains(key)).cloned())
    }

    async fn trust_workspace(
        &self,
        key: &crate::workspace_trust::TrustKey,
    ) -> Result<(), StoreError> {
        let mut trusted = lock(&self.trusted);
        if !trusted.contains(key) {
            trusted.push(key.clone());
        }
        Ok(())
    }
}

#[async_trait]
impl ArtifactStore for MemoryStore {
    async fn put(
        &self,
        _session: &SessionId,
        _purpose: ArtifactPurpose,
        _media_type: String,
        _bytes: Vec<u8>,
    ) -> Result<ArtifactRef, StoreError> {
        Err(StoreError::Unsupported {
            what: "memory artifacts".to_owned(),
        })
    }

    async fn get(&self, _artifact: &ArtifactRef) -> Result<Vec<u8>, StoreError> {
        Err(StoreError::Unsupported {
            what: "memory artifacts".to_owned(),
        })
    }
}

/// 内存里的 Artifact（`MemoryStore` 的 artifact 不可用，测试 checkpoint 与落盘时用这个）。
///
/// 与 NodeStore 一样核对会话与大小（缺陷修复 F18：以前只按 id 取、不核对大小，Read 自己造的 `size: 0` 引用在
/// 单测里能读，生产路径上读不回）。
#[derive(Default)]
pub struct MemoryArtifacts {
    objects: Mutex<HashMap<ArtifactId, (ArtifactRef, Vec<u8>)>>,
    next: AtomicU64,
}

impl MemoryArtifacts {
    pub fn count(&self) -> usize {
        lock(&self.objects).len()
    }
}

#[async_trait]
impl ArtifactStore for MemoryArtifacts {
    async fn put(
        &self,
        session: &SessionId,
        purpose: ArtifactPurpose,
        media_type: String,
        bytes: Vec<u8>,
    ) -> Result<ArtifactRef, StoreError> {
        let id = ArtifactId::new(format!(
            "tool-result-{}",
            self.next.fetch_add(1, Ordering::Relaxed) + 1
        ));
        let artifact = ArtifactRef {
            session: session.clone(),
            id: id.clone(),
            purpose,
            media_type,
            size: bytes.len() as u64,
        };
        lock(&self.objects).insert(id, (artifact.clone(), bytes));
        Ok(artifact)
    }

    async fn get(&self, artifact: &ArtifactRef) -> Result<Vec<u8>, StoreError> {
        let objects = lock(&self.objects);
        let Some((_, bytes)) = objects
            .get(&artifact.id)
            .filter(|(stored, _)| stored.session == artifact.session)
        else {
            return Err(StoreError::Storage {
                message: format!("artifact {} not found", artifact.id),
            });
        };
        if bytes.len() as u64 != artifact.size {
            return Err(StoreError::Storage {
                message: format!(
                    "artifact {} has {} bytes, expected {}",
                    artifact.id,
                    bytes.len(),
                    artifact.size
                ),
            });
        }
        Ok(bytes.clone())
    }

    async fn find(
        &self,
        session: &SessionId,
        id: &ArtifactId,
    ) -> Result<Option<ArtifactRef>, StoreError> {
        Ok(lock(&self.objects)
            .get(id)
            .filter(|(stored, _)| &stored.session == session)
            .map(|(stored, _)| stored.clone()))
    }
}

/// 最后一条开启新一轮的输入之后，最后一次回合结束是错误（node-store 按 `turn_usage` 的同一口径）。
fn last_turn_failed(facts: &[Fact]) -> bool {
    let mut failed = false;
    for fact in facts {
        match fact {
            Fact::MessageAppended {
                message: Message::User(user),
            } if crate::user_input::opens_turn(user) => failed = false,
            Fact::TurnEnded { status } => {
                failed = *status == crate::metering::CallStatus::Error;
            }
            _ => {}
        }
    }
    failed
}

/// 最后一次写入的标题（用户改名与生成的标题）。
fn last_title(facts: &[Fact]) -> Option<String> {
    facts.iter().rev().find_map(|fact| match fact {
        Fact::TitleChanged { title } | Fact::TitleGenerated { title, .. } => Some(title.clone()),
        _ => None,
    })
}

/// 第一条人类输入的文字（node-store 取首条真实用户输入的第一个文字 part）。
fn first_prompt(facts: &[Fact]) -> Option<String> {
    facts.iter().find_map(|fact| match fact {
        Fact::MessageAppended {
            message: Message::User(user),
        } if user.origin == crate::message::UserOrigin::Human => {
            user.content.iter().find_map(|block| match block {
                crate::message::ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
        }
        _ => None,
    })
}
