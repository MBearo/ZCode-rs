//! App 协议的方法表（docs/specs/rust-cli/app-protocol.md §3.4、§10.3）：P1 的方法；其余回 `-32601`。
//! 命令在 [`crate::commands`]，订阅在 [`crate::subscribe`]。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, SessionId};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{Kernel, KernelError};
use rust_cli_protocol_core::connection::{Reply, Service};
use rust_cli_protocol_core::rpc::RpcError;
use rust_cli_protocol_core::sessions::{Bridged, SessionBridge};
use serde_json::{Value, json};

use crate::context::Context;
use crate::inbound::Inbound;
use crate::inbox::Remembered;
use crate::index::Topics;
use crate::live::Live;
use crate::projection::state::Followup;

/// 会话不可用（`zcodeProtocolErrorCodes.sessionUnavailable`）。
pub const SESSION_UNAVAILABLE: i64 = -32004;

/// 会话事件攒成一批再发的窗口。
const EVENT_BATCH_WINDOW: std::time::Duration = std::time::Duration::from_millis(30);

/// 新会话的缺省（入口给出）。
#[derive(Debug, Clone)]
pub struct Defaults {
    pub selection: ModelSelection,
    pub mode: PermissionMode,
    pub environment: EnvironmentId,
}

/// 工作区级的开关（Host 推来的偏好；P1 只保存与回声）。
#[derive(Debug, Clone, Default)]
struct Preferences {
    ask_auto_resolution: bool,
    full_retention: bool,
}

pub struct Server {
    pub(crate) context: Arc<Context>,
    pub(crate) kernel: Arc<dyn Kernel>,
    pub(crate) bridge: SessionBridge<Live>,
    pub(crate) topics: Arc<Topics>,
    pub(crate) remembered: Mutex<Remembered>,
    pub(crate) followups: Mutex<HashMap<SessionId, Followup>>,
    pub(crate) defaults: Defaults,
    /// 本进程的实例标识（时钟探测的 `clock.instanceId`）。
    pub(crate) instance: String,
    preferences: Mutex<Preferences>,
}

pub(crate) fn kernel_error(error: &KernelError) -> RpcError {
    match error {
        KernelError::NotFound { session } => RpcError::new(
            SESSION_UNAVAILABLE,
            format!("Session unavailable: {}", session.as_str()),
        ),
        other => RpcError::internal(other.to_string()),
    }
}

impl Server {
    pub fn new(
        context: Arc<Context>,
        kernel: Arc<dyn Kernel>,
        spawner: rust_cli_base::Spawner,
        defaults: Defaults,
    ) -> Self {
        let topics = Arc::new(Topics::new(context.clone(), &spawner));
        Self {
            instance: context.fresh("instance"),
            bridge: SessionBridge::new(kernel.clone(), spawner, EVENT_BATCH_WINDOW),
            context,
            kernel,
            topics,
            remembered: Mutex::new(Remembered::default()),
            followups: Mutex::new(HashMap::new()),
            defaults,
            preferences: Mutex::new(Preferences::default()),
        }
    }

    pub(crate) fn workspace(&self, path: &str) -> WorkspaceRef {
        WorkspaceRef {
            environment: self.defaults.environment.clone(),
            root: EnvPath::new(path),
        }
    }

    /// 打开会话并接上投影（已经打开时直接给出）。
    pub(crate) async fn live(
        &self,
        session: &SessionId,
    ) -> Result<Arc<Bridged<Live>>, KernelError> {
        let followup = self
            .followups
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(session)
            .copied()
            .unwrap_or(Followup::Queue);
        let (context, topics) = (self.context.clone(), self.topics.clone());
        self.bridge
            .open(session, move |_, view| {
                Live::new(context, topics, view, followup)
            })
            .await
    }

    fn preferences(&self) -> std::sync::MutexGuard<'_, Preferences> {
        self.preferences
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    async fn read_presentation(&self, params: &Value) -> Value {
        let workspace = params["workspace"].clone();
        let path = workspace["workspacePath"].as_str().unwrap_or_default();
        let commands: Vec<Value> = self
            .kernel
            .user_commands(&self.workspace(path))
            .await
            .into_iter()
            .filter(|command| !command.hidden && !command.name.is_empty())
            .map(|command| {
                let mut value = json!({"name": command.name, "description": command.description,
                    "source": "custom"});
                if let Some(hint) = command.argument_hint {
                    value["inputHint"] = json!(hint);
                }
                value
            })
            .collect();
        json!({"workspace": workspace, "mode": self.defaults.mode.node_name(),
            "slashCommands": commands})
    }

    async fn close_session(&self, params: &Value) -> Value {
        let session = SessionId::new(params["sessionId"].as_str().unwrap_or_default());
        self.bridge.close(&session);
        json!({"closed": true})
    }

    async fn dispatch(&self, method: &str, params: Value) -> Result<Reply, RpcError> {
        let reply = |value: Value| Ok(Reply::new(value));
        match method {
            // 不支持的能力给 false（app-protocol.md §4.3）。
            "runtime/capabilities" => reply(json!({
                "independentPlanState": false,
                "historyFirst": false,
                "sessionDirectorySource": false,
                "genUiContext": false,
            })),
            // platform 自己读同一批文件；回执版本（app-protocol.md §5.1）。
            "provider/updateAccountConfig" => reply(json!({
                "receivedRevision": params["revision"],
                "providerCount": params["providers"].as_object().map_or(0, serde_json::Map::len),
                "status": "received",
            })),
            "provider/refreshRegistry" => reply(json!({"refreshed": true})),
            "workspace/updateInteractionPreferences" => {
                let enabled = params["preferences"]["askUserQuestionAutoResolutionEnabled"]
                    .as_bool()
                    .unwrap_or(false);
                self.preferences().ask_auto_resolution = enabled;
                reply(json!({"workspace": params["workspace"],
                    "askUserQuestionAutoResolutionEnabled": enabled, "snoozedInteractionCount": 0}))
            }
            "workspace/updateModelIoPreferences" => {
                let enabled = params["preferences"]["fullRetentionEnabled"]
                    .as_bool()
                    .unwrap_or(false);
                self.preferences().full_retention = enabled;
                reply(
                    json!({"workspace": params["workspace"], "fullRetentionEnabled": enabled,
                    "updatedSessionCount": 0}),
                )
            }
            "workspace/updateOffPeakToolPolicy" => {
                reply(json!({"workspace": params["workspace"], "enabled": params["enabled"]}))
            }
            // 动态工作流不移植（P4 决定）：回 disabled 生效。
            "workspace/updateDynamicWorkflowPolicy" => {
                reply(json!({"workspace": params["workspace"], "enabled": false}))
            }
            "workspace/readPresentation" => reply(self.read_presentation(&params).await),
            // 目录来源按 sessionIds 查父链：rust 的会话没有要报的父链（子代理不进列表），P1 回空列表。
            "session/list" => reply(json!({"sessions": []})),
            "session/read" => crate::legacy::read(self, &params).await.map(Reply::new),
            "session/close" => reply(self.close_session(&params).await),
            "v4/conversation/subscribe" => crate::subscribe::subscribe(self, &params).await,
            "v4/conversation/unsubscribe" => crate::subscribe::unsubscribe(self, &params).await,
            "v4/conversation/resync" => crate::subscribe::resync(self, &params).await,
            "v4/connection/flow" => crate::subscribe::flow(self, &params).await,
            "v4/sessionsIndex/reconcile" => crate::subscribe::reconcile(self, &params).await,
            "v4/command" => crate::commands::command(self, params).await.map(Reply::new),
            "v4/commands/query" => crate::commands::query(self, &params).await.map(Reply::new),
            other => Err(RpcError::method_not_found(other)),
        }
    }
}

/// [`Server`] 放在连接上的形状（读循环要 `'static`）。
pub struct Handler(pub Arc<Server>);

#[async_trait]
impl Service for Handler {
    async fn request(&self, method: &str, params: Value) -> Result<Reply, RpcError> {
        Inbound::get().method(method, &params)?;
        self.0.dispatch(method, params).await
    }
}
