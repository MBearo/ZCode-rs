use super::{Event, RunEvent};
use crate::{
    contract::{
        ClientMsg, Method, ModelIdentity, ModelPort, RuntimeClock, RuntimeError, RuntimePorts,
        ServerMsg, SessionStore, StorageCommitFailure, ToolPort,
    },
    domain::session::Session,
};
use anyhow::Result;
use serde_json::Value;
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Actor output: one ordered channel carrying replies, events and host requests.
pub type RuntimeOutput = mpsc::Sender<Vec<ServerMsg>>;
/// A request as the actor sees it; `token` echoes back in the reply.
pub(super) struct Call {
    pub token: u64,
    pub method: Method,
    pub params: Value,
}
pub(super) struct Active {
    pub selection: tokio::sync::watch::Sender<ModelIdentity>,
    pub cancel: CancellationToken,
    pub run_id: String,
    pub turn_id: String,
    /// Attribution of this run's model requests; the loop holds a copy.
    pub origin: std::sync::Arc<crate::contract::RequestOrigin>,
    /// Execution-scoped model and credentials (`sendText.modelExecution`).
    pub execution: Option<std::sync::Arc<super::submission::Execution>>,
    /// Permission inputs for this run's tool calls; the engine publishes updates.
    pub permissions: tokio::sync::watch::Sender<std::sync::Arc<super::permissions::Snapshot>>,
}
pub struct Engine {
    pub(super) child_updates:
        BTreeMap<String, tokio::sync::watch::Sender<crate::domain::subagent::Task>>,
    pub(super) uploads: crate::domain::attachment_upload::Uploads,
    pub(super) auxiliary: BTreeMap<String, super::auxiliary::Auxiliary>,
    pub(super) registry: Option<Arc<dyn crate::contract::ModelRegistry>>,
    pub(super) workspace_path: String,
    pub(super) workspace: String,
    pub(super) config: Option<ModelIdentity>,
    pub(super) store: Arc<dyn SessionStore>,
    pub(super) model: Option<Arc<dyn ModelPort>>,
    pub(super) context: Arc<dyn crate::contract::ContextPort>,
    pub(super) tools: Arc<dyn ToolPort>,
    pub(super) clock: Arc<dyn RuntimeClock>,
    pub(super) sessions: BTreeMap<String, Session>,
    pub(super) index: BTreeMap<String, Value>,
    pub(super) closed: std::collections::BTreeSet<String>,
    pub(super) session_access: BTreeMap<String, u64>,
    pub(super) access_seq: u64,
    pub(super) durable_acks: std::collections::BTreeSet<String>,
    pub(super) acks: BTreeMap<String, Value>,
    pub(super) active: BTreeMap<String, Active>,
    /// Mode fallbacks, project and session rules, config lists.
    pub(super) permissions: super::permissions::Permissions,
    /// Input options handed from admission to run start, keyed by (session, turn).
    pub(super) submissions: BTreeMap<(String, String), super::submission::Submission>,
    /// Everything waiting on an external answer; released per owner on every terminal path.
    pub(super) waiters: super::waiters::Waiters,
    /// Open delivery topics and their subscriber count; a conversation topic pins its session.
    pub(super) interest: BTreeMap<String, usize>,
    pub(super) epoch: String,
    pub(super) index_seq: u64,
    pub(super) config_seq: u64,
    pub(super) outbox: Vec<ServerMsg>,
    pub(super) auto_resolution_preference: bool,
    pub(super) question_timing: (u64, u64),
    pub(super) events: mpsc::Sender<RunEvent>,
    pub(super) event_rx: mpsc::Receiver<RunEvent>,
    /// User config hooks and workspace hook trust.
    pub(super) hooks: super::workspace_trust::HookState,
}
impl Engine {
    pub async fn new(
        workspace: String,
        config: Option<ModelIdentity>,
        ports: RuntimePorts,
    ) -> Result<Self> {
        let RuntimePorts {
            context,
            store,
            model,
            tools,
            clock,
        } = ports;
        // 仅未完成的文件事务需要冷读对应 session；普通启动仍然只读取 metadata 索引。
        for id in tools.pending_rewinds().await? {
            let session = store.load_session(&workspace, &id).await?;
            if let Some(session) = session {
                tools
                    .recover_rewind(&id, session.rewind_committed.as_deref())
                    .await?;
            }
        }
        let index = store.load_index(&workspace).await?;
        let permissions = super::permissions::Permissions::from_settings(
            &store.project_settings(&workspace).await?,
        );
        let (events, event_rx) = mpsc::channel(128);
        Ok(Self {
            child_updates: BTreeMap::new(),
            uploads: Default::default(),
            auxiliary: BTreeMap::new(),
            registry: None,
            workspace_path: workspace.clone(),
            workspace,
            config,
            store,
            model,
            tools,
            clock: clock.clone(),
            context,
            sessions: BTreeMap::new(),
            index,
            closed: Default::default(),
            session_access: Default::default(),
            access_seq: 0,
            durable_acks: Default::default(),
            acks: BTreeMap::new(),
            active: BTreeMap::new(),
            submissions: BTreeMap::new(),
            permissions,
            waiters: Default::default(),
            interest: BTreeMap::new(),
            epoch: clock.id(),
            index_seq: 0,
            config_seq: 0,
            outbox: vec![],
            auto_resolution_preference: true,
            question_timing: (60_000, 300_000),
            events,
            event_rx,
            hooks: Default::default(),
        })
    }
    /// Config file `permission` section (mode fallback, allowed/disallowed tools),
    /// read once at startup like Node's app creation.
    pub fn with_permission_config(mut self, permission: &Value) -> Self {
        self.permissions.config = crate::domain::permission::Config::from_config(permission);
        self.permissions.config_mode = permission["mode"].as_str().map(str::to_owned);
        self
    }
    pub fn with_registry(
        mut self,
        registry: Option<Arc<dyn crate::contract::ModelRegistry>>,
        workspace_path: String,
    ) -> Self {
        self.registry = registry;
        self.workspace_path = workspace_path;
        if let Some(registry) = &self.registry {
            self.config = registry.default_selection();
        }
        self
    }
    pub async fn serve(
        mut self,
        mut input: mpsc::Receiver<ClientMsg>,
        output: RuntimeOutput,
        cancel: CancellationToken,
    ) -> Result<()> {
        let mut refresh = tokio::time::interval(std::time::Duration::from_secs(1));
        refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let serving = async {
            loop {
                let question_delay = self.question_delay();
                tokio::select! {
                    _=async {if let Some(delay)=question_delay {tokio::time::sleep(delay).await} else {std::future::pending::<()>().await}}=>{self.advance_questions().await?;self.flush(&output).await?;},
                    _=cancel.cancelled()=>break,
                    message=input.recv()=>match message {
                        Some(ClientMsg::Request{token,method,params})=>self.request(Call{token,method,params},&output).await?,
                        Some(ClientMsg::HostReply{id,result})=> { if let Some(wait) = self.waiters.take_host(&id) { let _ = wait.reply.send(result); } },
                        Some(ClientMsg::TopicReleased{topic})=>{self.release_topic(&topic);self.trim_resident().await?;},
                        Some(ClientMsg::ConnectionClosed{connection})=>{self.uploads.clear_connection(&connection);self.trim_resident().await?;},
                        Some(ClientMsg::Eof) | None=>break,
                    },
                    Some(event)=self.event_rx.recv()=>{
                        let terminal=matches!(&event.event,Event::Finished {..}) || matches!(&event.event,Event::Background {task,..} if task.status!="running");
                        self.apply_event(event).await?;self.flush(&output).await?;
                        if terminal {self.trim_resident().await?;}
                    },
                    _=refresh.tick()=>{
                        self.uploads.prune(self.clock.now());
                        if let Some(registry) = &self.registry && registry.refresh(None).await.unwrap_or(false) { self.refresh_catalog()?; self.flush(&output).await?; }
                    },
                }
            }
            Ok::<_, anyhow::Error>(())
        };
        let mut result =
            tokio::select! {biased; _=cancel.cancelled()=>Ok(()), result=serving=>result};
        let mut storage_failed = result
            .as_ref()
            .err()
            .is_some_and(|e| e.is::<StorageCommitFailure>());
        if result.is_err() {
            self.child_updates.clear();
        }
        for active in self.active.values() {
            active.cancel.cancel();
        }
        for job in self.auxiliary.values() {
            job.cancel.cancel();
        }
        self.auxiliary.clear();
        for session in self.sessions.values_mut() {
            if let Some(goal) = &mut session.goal {
                goal.pause(self.clock.now());
            }
            session.auto_drain = false;
            session.queued_now = None;
        }
        self.waiters.clear();
        for id in self.sessions.keys() {
            self.tools.cancel_session(id, None).await?;
        }
        // 必须等待拥有的工具 task 收口后再释放 runtime；只 drop Tokio task 会漏掉 Shell 后代。
        // 清理期限由工具 adapter 的 TERM/KILL 生命周期拥有；不能在 1200ms
        // 提前 drop 仍处于 1500ms 宽限期的前台任务，留下工作进程或丢失终态。
        while !self.active.is_empty()
            || (!storage_failed
                && self
                    .sessions
                    .values()
                    .any(|s| s.background.values().any(|t| t.status == "running")))
        {
            match self.event_rx.recv().await {
                Some(event) => {
                    // 事务失败后的内存事实不可再提交；否则一次失败的 ACK/工具结果会被收口路径复活。
                    if storage_failed {
                        if matches!(event.event, Event::Finished { .. })
                            && self
                                .active
                                .get(&event.session_id)
                                .is_some_and(|active| active.run_id == event.run_id)
                        {
                            self.active.remove(&event.session_id);
                        }
                    } else if let Err(error) = self.apply_event(event).await {
                        storage_failed = true;
                        self.child_updates.clear();
                        result = Err(error);
                    }
                    self.outbox.clear();
                }
                _ => break,
            }
        }
        self.event_rx.close();
        self.tools.shutdown().await?;
        // EOF 后持久化中断态，再结束 actor；不等待挂起权限或再次启动队列。
        for session in self.sessions.values_mut() {
            if (session.running() || session.background.values().any(|t| t.status == "running"))
                && !storage_failed
            {
                session.recover(self.clock.id(), self.clock.now());
                self.store
                    .commit_receipt(&self.workspace, Some(session), None)
                    .await?;
            }
        }
        result
    }
    async fn request(&mut self, call: Call, output: &RuntimeOutput) -> Result<()> {
        self.outbox.clear();
        let token = call.token;
        // 这些请求在后台完成，稍后以同一 token 回复；这里只回复启动失败。
        let deferred = match call.method {
            Method::McpList => Some(self.start_mcp_query(&call)),
            Method::WorkspaceGenerateText | Method::ProviderTestModelConnectivity => {
                Some(self.start_auxiliary(&call))
            }
            _ => None,
        };
        if let Some(started) = deferred {
            if let Err(error) = started {
                // 保持既有 -32602 响应，启动失败均为请求校验问题。
                let error = RuntimeError::Coded {
                    code: -32602,
                    message: error.to_string(),
                };
                output
                    .send(vec![ServerMsg::Reply {
                        token,
                        result: Err(error),
                    }])
                    .await?;
            }
            return Ok(());
        }
        let p = &call.params;
        let result = match call.method {
            // 与 Node CommandInbox 一致：信封或 payload 不合法时回 rejected ACK，而非 RPC 错误。
            Method::Command => match crate::domain::protocol::validate_command(p) {
                Err(issue) => Ok(crate::domain::protocol::invalid_payload_ack(p, &issue)),
                Ok(()) => match serde_json::from_value(call.params.clone()) {
                    Ok(command) => self.command(command).await,
                    Err(_) => Err(RuntimeError::invalid_params("Invalid command envelope")),
                },
            },
            Method::TopicOpen => self.open_topic(p).await,
            Method::TopicSnapshot => self.topic_snapshot_value(p),
            Method::AttachmentBegin
            | Method::AttachmentChunk
            | Method::AttachmentCommit
            | Method::AttachmentAbort => self.attachment_upload(call.method.as_str(), p).await,
            Method::AttachmentRead
            | Method::AttachmentPreviewSource
            | Method::ConversationAttachmentRead
            | Method::ConversationAttachmentStat
            | Method::ConversationRowsRange
            | Method::ConversationPlans => self.conversation_query(call.method, p).await,
            Method::WorkspaceUpdateInteractionPreferences => self.interaction_preferences(p).await,
            Method::WorkspaceHooksTrustGrant => self.trust_grant(p).await,
            Method::ConversationFileChanges => self.file_changes(p).await,
            Method::ConversationFileRewindPreview => self.rewind_preview(p).await,
            Method::SessionRead => self.read_cold_session(p).await,
            Method::CommandsQuery => self.query_acks(p).await,
            Method::SessionList => self.list_sessions(p).await,
            Method::SessionSubagents => self.subagents_query(p).await,
            Method::SessionClose => self.close_runtime_session(p).await,
            Method::SkillsReferenceCatalog => self.skill_catalog(p).await,
            Method::SessionCreate => self.legacy_create(p).await,
            Method::SessionResume => self.legacy_resume(p).await,
            Method::SessionSubscribe => self.legacy_subscribe(p),
            Method::SessionDebug => self.session_debug(p),
            Method::SessionSetModel | Method::SessionSetThoughtLevel | Method::SessionSetMode => {
                self.legacy_setter(call.method, p).await
            }
            Method::ProviderUpdateAccountConfig => self.update_account(p).await,
            method => self.query(method, p),
        };
        let storage_failed = result
            .as_ref()
            .err()
            .is_some_and(|e| e.is::<StorageCommitFailure>());
        let result = result.map_err(|error| {
            // 失败请求不能发布半途产生的事件。
            self.outbox.clear();
            let error = RuntimeError::classify(&error);
            // 只记录方法与错误码：错误文本可能包含用户路径或内容，不写入日志。
            tracing::warn!(
                target: "zcode::runtime",
                event = "rpc.request.failed",
                method = call.method.as_str(),
                code = error.code(),
                "Request failed"
            );
            error
        });
        let mut batch = vec![ServerMsg::Reply { token, result }];
        batch.append(&mut self.outbox);
        output.send(batch).await?;
        if storage_failed {
            tracing::error!(
                target: "zcode::runtime",
                event = "storage.commit.failed",
                method = call.method.as_str(),
                "Storage commit failed; stopping runtime"
            );
            return Err(StorageCommitFailure.into());
        }
        self.trim_resident().await?;
        Ok(())
    }
    async fn flush(&mut self, output: &RuntimeOutput) -> Result<()> {
        if !self.outbox.is_empty() {
            output.send(std::mem::take(&mut self.outbox)).await?;
        }
        Ok(())
    }
}
