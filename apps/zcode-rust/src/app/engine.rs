use super::{Event, RunEvent, subscriptions::Subscription};
use crate::{
    contract::{
        Input, ModelIdentity, ModelPort, Output, RuntimeClock, RuntimePorts, SessionStore,
        StorageCommitFailure, ToolPort,
    },
    domain::{
        protocol::{Request, rpc_error},
        session::Session,
    },
};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

pub(super) struct Active {
    pub selection: tokio::sync::watch::Sender<ModelIdentity>,
    pub cancel: CancellationToken,
    pub run_id: String,
    pub turn_id: String,
}
pub struct Engine {
    pub(super) child_updates:
        BTreeMap<String, tokio::sync::watch::Sender<crate::domain::subagent::Task>>,
    pub(super) uploads: crate::domain::attachment_upload::Uploads,
    pub(super) auxiliary: BTreeMap<String, super::auxiliary::Auxiliary>,
    pub(super) registry: Option<Arc<dyn crate::contract::ModelRegistry>>,
    pub(super) workspace_path: String,
    pub(super) auth: BTreeMap<String, (String, String, Value, oneshot::Sender<Value>)>,
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
    pub(super) permissions: BTreeMap<String, (String, oneshot::Sender<bool>)>,
    pub(super) subscriptions: BTreeMap<String, Subscription>,
    pub(super) epoch: String,
    pub(super) index_seq: u64,
    pub(super) config_seq: u64,
    pub(super) outbox: Vec<Value>,
    pub(super) auto_resolution_preference: bool,
    pub(super) questions: BTreeMap<String, super::questions::WaitingQuestion>,
    pub(super) question_timing: (u64, u64),
    pub(super) events: mpsc::Sender<RunEvent>,
    pub(super) event_rx: mpsc::Receiver<RunEvent>,
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
        let (events, event_rx) = mpsc::channel(128);
        Ok(Self {
            child_updates: BTreeMap::new(),
            uploads: Default::default(),
            auxiliary: BTreeMap::new(),
            registry: None,
            workspace_path: workspace.clone(),
            auth: BTreeMap::new(),
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
            permissions: BTreeMap::new(),
            subscriptions: BTreeMap::new(),
            epoch: clock.id(),
            index_seq: 0,
            config_seq: 0,
            outbox: vec![],
            auto_resolution_preference: true,
            questions: BTreeMap::new(),
            question_timing: (60_000, 300_000),
            events,
            event_rx,
        })
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
        mut input: mpsc::Receiver<Input>,
        output: Output,
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
                        Some(Input::Request(request))=>self.request(request,&output).await?,
                        Some(Input::Response{id,result})=> { if let Some((_,_,_,reply)) = self.auth.remove(&id) { let _ = reply.send(result); } },
                        Some(Input::Invalid)=>output.send(vec![rpc_error(&None,-32700,"Invalid protocol request")]).await?,
                        Some(Input::TooLarge)=>{output.send(vec![rpc_error(&None,-32600,"Request exceeds size limit")]).await?;break;},
                        _=>break,
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
        self.permissions.clear();
        self.questions.clear();
        self.auth.clear();
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
                    .commit(&self.workspace, Some(session), None)
                    .await?;
            }
        }
        result
    }
    async fn request(&mut self, request: Request, output: &Output) -> Result<()> {
        self.outbox.clear();
        if request.method == "mcp/list" {
            if let Err(error) = self.start_mcp_query(&request) {
                output
                    .send(vec![rpc_error(&request.id, -32602, &error.to_string())])
                    .await?;
            }
            return Ok(());
        }
        if matches!(
            request.method.as_str(),
            "workspace/generateText" | "provider/testModelConnectivity"
        ) {
            if let Err(error) = self.start_auxiliary(&request) {
                output
                    .send(vec![rpc_error(&request.id, -32602, &error.to_string())])
                    .await?;
            }
            return Ok(());
        }
        let result = match request.method.as_str() {
            "v4/command" => match serde_json::from_value(request.params.clone()) {
                Ok(command) => self.command(command).await,
                Err(_) => {
                    output
                        .send(vec![rpc_error(
                            &request.id,
                            -32602,
                            "Invalid command envelope",
                        )])
                        .await?;
                    return Ok(());
                }
            },
            "v4/conversation/subscribe" => self.subscribe(&request.params).await,
            "v4/conversation/resync" => self.resync(&request.params),
            "v4/conversation/unsubscribe" => self.unsubscribe(&request.params),
            "v4/connection/flow" => self.connection_flow(&request.params),
            "v4/attachment/begin"
            | "v4/attachment/chunk"
            | "v4/attachment/commit"
            | "v4/attachment/abort" => {
                self.attachment_upload(&request.method, &request.params)
                    .await
            }
            "v4/attachment/read"
            | "v4/attachment/previewSource"
            | "v4/conversation/attachmentRead"
            | "v4/conversation/attachmentStat"
            | "v4/conversation/rowsRange"
            | "v4/conversation/plans" => {
                self.conversation_query(&request.method, &request.params)
                    .await
            }
            "workspace/updateInteractionPreferences" => {
                self.interaction_preferences(&request.params).await
            }
            "v4/conversation/fileChanges" => self.file_changes(&request.params).await,
            "v4/conversation/fileRewindPreview" => self.rewind_preview(&request.params).await,
            "session/read" => self.read_cold_session(&request.params).await,
            "v4/commands/query" => self.query_acks(&request.params).await,
            "session/list" => self.list_sessions(&request.params).await,
            "session/subagents" => self.subagents_query(&request.params).await,
            "skills/referenceCatalog" => self.skill_catalog(&request.params).await,
            "session/create" => self.import_shared_context(&request.params).await,
            "provider/updateAccountConfig" => self.update_account(&request.params).await,
            _ => self.query(&request.method, &request.params),
        };
        let storage_failed = result
            .as_ref()
            .err()
            .is_some_and(|e| e.is::<StorageCommitFailure>());
        let response = match result {
            Ok(value) => json!({"id":request.id,"result":value}),
            Err(error) => {
                self.outbox.clear();
                let message = error.to_string();
                rpc_error(
                    &request.id,
                    if message.starts_with("Unsupported method:") {
                        -32601
                    } else {
                        -32602
                    },
                    &message,
                )
            }
        };
        let mut batch = vec![];
        if request.id.is_some() {
            batch.push(response);
        }
        batch.append(&mut self.outbox);
        if !batch.is_empty() {
            output.send(batch).await?;
        }
        if storage_failed {
            return Err(StorageCommitFailure.into());
        }
        self.trim_resident().await?;
        Ok(())
    }
    async fn flush(&mut self, output: &Output) -> Result<()> {
        if !self.outbox.is_empty() {
            output.send(std::mem::take(&mut self.outbox)).await?;
        }
        Ok(())
    }
    pub(super) async fn persist(&mut self, id: &str, ack: Option<(String, Value)>) -> Result<()> {
        if let Some(session) = self.sessions.get_mut(id) {
            session.resident_bytes = None;
        }
        // 导入候选还没有可见 row，但它已是 durable session；只有真正 draft 可以跳过提交。
        if self.sessions.get(id).is_some_and(|s| s.phase == "draft") {
            return Ok(());
        }
        let mut keys = self
            .sessions
            .get(id)
            .map(|s| s.pending_acks.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        if let Some((key, _)) = &ack {
            keys.push(key.clone());
        }
        if let Some((key, _)) = self.sessions.get(id).and_then(|s| s.creation_ack.as_ref()) {
            keys.push(key.clone());
        }
        self.store
            .commit(&self.workspace, self.sessions.get_mut(id), ack)
            .await
            .context(StorageCommitFailure)?;
        self.durable_acks.extend(keys);
        Ok(())
    }
}
