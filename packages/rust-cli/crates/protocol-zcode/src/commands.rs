//! 命令的处理（[`crate::inbox`] 的裁决与转交）：P1 的五个命令。

use rust_cli_base::sync::LockUnpoisoned;
use rust_cli_kernel_api::ids::{CommandId, InteractionId, SessionId};
use rust_cli_kernel_api::interaction::PermissionAnswer;
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::orchestration::InteractionRequest;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{
    Ack as KernelAck, Command, CommandEnvelope, CreateSession, Delivery, SessionHandle,
};
use rust_cli_protocol_core::rpc::RpcError;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::inbound::Inbound;
use crate::inbox::{self, Ack, Envelope, Status};
use crate::projection::InputMeta;
use crate::projection::state::{Followup, Phase};
use crate::server::{Server, kernel_error};

/// App 的模型选择（`modelSelectionSchema`）。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppSelection {
    provider_id: String,
    model_id: String,
    #[serde(default)]
    options: AppSelectionOptions,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppSelectionOptions {
    reasoning_level: Option<String>,
}

/// App 的模型选择 → 内核的选择。原样留着 JSON（排队项照原样带回去），用时再解。
fn selection(value: Option<&Value>) -> Option<ModelSelection> {
    let chosen: AppSelection = serde_json::from_value(value?.clone()).ok()?;
    Some(ModelSelection {
        reasoning: chosen.options.reasoning_level,
        ..ModelSelection::new(chosen.provider_id, chosen.model_id)
    })
}

/// App 的模式与 plan 开关 → 内核的权限模式与 plan（README §5 的映射；`plan` 是旧写法，读成 plan 开、build）。
fn mode(name: Option<&str>, plan: Option<bool>) -> Option<(PermissionMode, bool)> {
    let name = name?;
    if name == "plan" {
        return Some((PermissionMode::Default, true));
    }
    Some((PermissionMode::from_node_name(name)?, plan.unwrap_or(false)))
}

/// 一次输入（sendText 的 payload、createSession 的 firstInput；只取用到的字段）。
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Input {
    text: Option<String>,
    model_selection: Option<Value>,
    mode: Option<String>,
    plan_enabled: Option<bool>,
    input_origin: Option<String>,
    requested_delivery: Option<Requested>,
}

/// sendText 要的投递（`requestedDelivery`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
enum Requested {
    StartNow,
    Queue,
    Guide,
}

impl Requested {
    fn as_str(self) -> &'static str {
        match self {
            Self::StartNow => "startNow",
            Self::Queue => "queue",
            Self::Guide => "guide",
        }
    }
}

/// createSession 的 payload。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateSessionPayload {
    session_id: Option<String>,
    workspace_id: Option<String>,
    first_input: Option<Input>,
    #[serde(default)]
    config: SessionConfig,
}

/// createSession 的 `config`。
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionConfig {
    model_selection: Option<Value>,
    mode: Option<String>,
    plan_enabled: Option<bool>,
    followup_mode: Option<Followup>,
}

/// resolveInteraction 的 payload。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResolvePayload {
    interaction_id: String,
    answer: AnswerPayload,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AnswerPayload {
    option_id: Option<String>,
    free_text: Option<String>,
}

/// setFollowupMode 的 payload。
#[derive(Deserialize)]
struct FollowupPayload {
    mode: Followup,
}

/// 解 payload（已经过 schema，失败只会是 schema 与这里的结构不一致）：解不出时按 payload 不合规拒绝。
fn payload<P: DeserializeOwned>(envelope: &Envelope) -> Result<P, Box<Ack>> {
    serde_json::from_value(envelope.payload.clone()).map_err(|error| {
        Box::new(Ack::refused(
            &envelope.command_id,
            Status::Rejected,
            "proto.invalidPayload",
            format!("Invalid {} payload: {error}", envelope.kind),
            0,
        ))
    })
}

/// 内核的回执 → 命令的 ACK：接受是 `accepted`，重复是同一个标成 duplicate，拒绝按 `refused` 给出。
fn ack_of(kernel: KernelAck, accepted: Ack, refused: impl FnOnce(String) -> Ack) -> Ack {
    match kernel {
        KernelAck::Accepted { .. } => accepted,
        KernelAck::Duplicate => inbox::duplicate(&accepted),
        KernelAck::Rejected { reason } => refused(reason),
    }
}

async fn send(handle: &dyn SessionHandle, id: &str, command: Command) -> KernelAck {
    handle
        .send(CommandEnvelope {
            id: CommandId::new(id),
            command,
        })
        .await
}

fn unsupported(envelope: &Envelope, revision: u64) -> Ack {
    Ack::refused(
        &envelope.command_id,
        Status::Rejected,
        "proto.unsupportedCommand",
        format!("{} is not supported by this agent yet", envelope.kind),
        revision,
    )
}

pub async fn command(server: &Server, params: Value) -> Result<Value, RpcError> {
    let envelope: Envelope = serde_json::from_value(params.clone())
        .map_err(|error| RpcError::invalid_params(format!("Invalid command envelope: {error}")))?;
    let key = inbox::key(envelope.session_id.as_deref(), &envelope.command_id);
    let known = server.remembered.lock_unpoisoned().get(&key).cloned();
    if let Some(known) = known {
        return Ok(inbox::duplicate(&known).to_value());
    }
    let ack = match Inbound::get().command(&params) {
        Err(message) => Ack::refused(
            &envelope.command_id,
            Status::Rejected,
            "proto.invalidPayload",
            message,
            0,
        ),
        Ok(()) => decide(server, &envelope).await,
    };
    if ack.status != Status::Duplicate {
        server.remembered.lock_unpoisoned().put(key, ack.clone());
    }
    // 命令事实（G2）：会话命令照 Node 的格式落库；写失败只记日志，ACK 照常回。
    if let Some(session) = &envelope.session_id
        && ack.is_fact()
        && let Err(error) = server
            .kernel
            .queries()
            .put_record(&SessionId::new(session), &inbox::fact(&ack))
            .await
    {
        tracing::warn!(event = "protocol.command_fact.failed", error = %error, "writing the command fact failed");
    }
    Ok(ack.to_value())
}

async fn decide(server: &Server, envelope: &Envelope) -> Ack {
    if envelope.kind == "createSession" {
        return create_session(server, envelope).await;
    }
    let Some(session) = envelope.session_id.as_deref() else {
        return Ack::refused(
            &envelope.command_id,
            Status::Rejected,
            "proto.sessionRequired",
            "this command needs a sessionId",
            0,
        );
    };
    let live = match server.live(&SessionId::new(session)).await {
        Ok(live) => live,
        Err(error) => {
            return Ack::refused(
                &envelope.command_id,
                Status::Rejected,
                "proto.sessionNotFound",
                kernel_error(&error).message,
                0,
            );
        }
    };
    let (phase, revision) = live.sink.status();
    let handle = live.handle.as_ref();
    match envelope.kind.as_str() {
        "sendText" => send_text(handle, &live.sink, envelope, phase, revision).await,
        "stop" => ack_of(
            send(handle, &envelope.command_id, Command::Interrupt).await,
            Ack::accepted(&envelope.command_id, revision, None),
            |reason| {
                Ack::refused(
                    &envelope.command_id,
                    Status::Noop,
                    "proto.nothingToStop",
                    reason,
                    revision,
                )
            },
        ),
        "resolveInteraction" => resolve(handle, &live.sink, envelope, revision).await,
        "setFollowupMode" => set_followup(server, handle, &live, envelope).await,
        _ => unsupported(envelope, revision),
    }
}

async fn create_session(server: &Server, envelope: &Envelope) -> Ack {
    let payload: CreateSessionPayload = match payload(envelope) {
        Ok(payload) => payload,
        Err(ack) => return *ack,
    };
    let config = &payload.config;
    let first = payload.first_input.unwrap_or_default();
    let chosen = selection(config.model_selection.as_ref())
        .or_else(|| selection(first.model_selection.as_ref()))
        .unwrap_or_else(|| server.defaults.selection.clone());
    let (permission, plan) =
        mode(config.mode.as_deref(), config.plan_enabled).unwrap_or((server.defaults.mode, false));
    let id = payload
        .session_id
        .as_deref()
        .map_or_else(inbox::new_session_id, SessionId::new);
    if let Some(followup) = config.followup_mode {
        server
            .followups
            .lock_unpoisoned()
            .insert(id.clone(), followup);
    }
    let workspace = payload.workspace_id.as_deref().unwrap_or_default();
    let created = server
        .kernel
        .create_session(CreateSession {
            id: Some(id.clone()),
            workspace: server.workspace(workspace),
            selection: chosen,
            mode: permission,
            plan,
        })
        .await;
    let refused = |message: String| {
        Ack::refused(
            &envelope.command_id,
            Status::Failed,
            "fault.command.createSessionFailed",
            message,
            0,
        )
    };
    let id = match created {
        Ok(id) => id,
        Err(error) => return refused(error.to_string()),
    };
    let live = match server.live(&id).await {
        Ok(live) => live,
        Err(error) => return refused(error.to_string()),
    };
    crate::subscribe::announce(&live);
    let mut result = json!({"type": "createSession", "sessionId": id.as_str()});
    if let Some(text) = first.text.clone() {
        live.sink.remember_input(
            &envelope.command_id,
            input_meta(envelope, &first, "startNow", "startNow"),
        );
        let submit = Command::Submit {
            text,
            attachments: Vec::new(),
            delivery: Delivery::Queue,
        };
        if let KernelAck::Accepted { .. } =
            send(live.handle.as_ref(), &envelope.command_id, submit).await
        {
            result["input"] = json!({"delivery": "startNow", "inputId": envelope.command_id});
        }
    }
    Ack::accepted(&envelope.command_id, 0, Some(result))
}

fn input_meta(
    envelope: &Envelope,
    input: &Input,
    requested: &'static str,
    admitted: &'static str,
) -> InputMeta {
    InputMeta {
        client: envelope.client_id.clone(),
        origin: envelope
            .input_origin
            .clone()
            .or_else(|| input.input_origin.clone()),
        text: input.text.clone().unwrap_or_default(),
        selection: input.model_selection.clone(),
        mode: input.mode.clone(),
        plan: input.plan_enabled,
        admitted_at: 0,
        requested,
        admitted,
    }
}

async fn send_text(
    handle: &dyn SessionHandle,
    live: &crate::live::Live,
    envelope: &Envelope,
    phase: Phase,
    revision: u64,
) -> Ack {
    let input: Input = match payload(envelope) {
        Ok(input) => input,
        Err(ack) => return *ack,
    };
    let command = envelope.command_id.as_str();
    // 这次提交带的模型与模式先作用到会话（同 Node：Submission 带完整的选择）。
    let view = handle.view().await.ok();
    if let Some(chosen) = selection(input.model_selection.as_ref())
        && view.as_ref().is_none_or(|view| view.selection != chosen)
    {
        send(
            handle,
            &format!("{command}:model"),
            Command::SetModel { selection: chosen },
        )
        .await;
    }
    if let Some((permission, plan)) = mode(input.mode.as_deref(), input.plan_enabled) {
        if view.as_ref().is_none_or(|view| view.mode != permission) {
            send(
                handle,
                &format!("{command}:mode"),
                Command::SetMode { mode: permission },
            )
            .await;
        }
        if view.as_ref().is_none_or(|view| view.plan != plan) {
            send(
                handle,
                &format!("{command}:plan"),
                Command::SetPlan { enabled: plan },
            )
            .await;
        }
    }
    let running = phase == Phase::Running;
    let requested = input.requested_delivery;
    let followup = live.followup();
    let (delivery, admitted) = match (requested, running) {
        (Some(Requested::Guide), true) => (Delivery::Steer, "guide"),
        (None, true) if followup == Followup::Guide => (Delivery::Steer, "guide"),
        (_, true) => (Delivery::Queue, "queue"),
        (_, false) => (Delivery::Queue, "startNow"),
    };
    let requested = requested.unwrap_or(Requested::StartNow).as_str();
    live.remember_input(command, input_meta(envelope, &input, requested, admitted));
    let submit = Command::Submit {
        text: input.text.unwrap_or_default(),
        attachments: Vec::new(),
        delivery,
    };
    let result = json!({"type": "inputAccepted", "delivery": admitted, "inputId": command});
    ack_of(
        send(handle, command, submit).await,
        Ack::accepted(command, revision, Some(result)),
        |reason| {
            Ack::refused(
                command,
                Status::Rejected,
                "guard.inputRejected",
                reason,
                revision,
            )
        },
    )
}

async fn resolve(
    handle: &dyn SessionHandle,
    live: &crate::live::Live,
    envelope: &Envelope,
    revision: u64,
) -> Ack {
    let resolved: ResolvePayload = match payload(envelope) {
        Ok(resolved) => resolved,
        Err(ack) => return *ack,
    };
    let interaction = resolved.interaction_id.as_str();
    let option = resolved.answer.option_id.as_deref();
    let pending = handle.view().await.ok().and_then(|view| {
        view.pending
            .into_iter()
            .find(|(id, _)| id.as_str() == interaction)
            .map(|(_, request)| request)
    });
    let Some(InteractionRequest::Permission {
        suggestions,
        tool_use: tool,
        ..
    }) = pending
    else {
        return Ack::refused(
            &envelope.command_id,
            Status::Noop,
            "proto.alreadyResolved",
            "the interaction is no longer pending",
            revision,
        );
    };
    // “完全访问”：先把会话切到 yolo（之后不再询问），再放行这一次。
    if option == Some("fullAccess") {
        send(
            handle,
            &format!("{}:mode", envelope.command_id),
            Command::SetMode {
                mode: PermissionMode::BypassPermissions,
            },
        )
        .await;
    }
    let answer = match option {
        Some("allowOnce" | "fullAccess") => PermissionAnswer::Allow {
            remember: Vec::new(),
        },
        Some("allowAlways") => PermissionAnswer::Allow {
            remember: suggestions,
        },
        _ => PermissionAnswer::Deny {
            feedback: resolved
                .answer
                .free_text
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_owned),
        },
    };
    if let PermissionAnswer::Deny { .. } = &answer {
        live.deny(&tool);
    }
    let command = Command::ResolveInteraction {
        interaction: InteractionId::new(interaction),
        answer: answer.into(),
    };
    // 与 Node 相同：ACK 不带 result（答案已经进了会话，投影随事件更新）。
    ack_of(
        send(handle, &envelope.command_id, command).await,
        Ack::accepted(&envelope.command_id, revision, None),
        |reason| {
            Ack::refused(
                &envelope.command_id,
                Status::Noop,
                "proto.alreadyResolved",
                reason,
                revision,
            )
        },
    )
}

/// setFollowupMode（CAS）：跟进方式是协议保存的配置；裁决用会话 actor 的当前 revision（G1）。
async fn set_followup(
    server: &Server,
    handle: &dyn SessionHandle,
    live: &std::sync::Arc<rust_cli_protocol_core::sessions::Bridged<crate::live::Live>>,
    envelope: &Envelope,
) -> Ack {
    let command = envelope.command_id.as_str();
    let revision = match handle.view().await {
        Ok(view) => view.revision,
        Err(error) => {
            return Ack::refused(
                command,
                Status::Failed,
                "fault.command.sessionUnavailable",
                error.to_string(),
                0,
            );
        }
    };
    let wanted = match payload::<FollowupPayload>(envelope) {
        Ok(chosen) => chosen.mode,
        Err(_) => {
            return Ack::refused(
                command,
                Status::Rejected,
                "proto.invalidPayload",
                "unknown followup mode",
                revision,
            );
        }
    };
    if live.sink.followup() == wanted {
        return Ack::refused(
            command,
            Status::Noop,
            "proto.unchanged",
            "the followup mode is already set",
            revision,
        );
    }
    if envelope.base_revision != Some(revision) {
        return Ack::refused(
            command,
            Status::Stale,
            "proto.staleRevision",
            "the session changed",
            revision,
        );
    }
    if let Some(session) = &envelope.session_id {
        server
            .followups
            .lock_unpoisoned()
            .insert(SessionId::new(session), wanted);
    }
    // 增量在会话的泵任务里发（与内核事件的帧同一个顺序）。
    let sink = live.sink.clone();
    live.run(Box::pin(async move { sink.set_followup(wanted).await }));
    Ack::accepted(command, revision, None)
}

/// `v4/commands/query`：内存表，然后命令事实（G2）；都没有时 unknown。时钟探测带 `clock`。
pub async fn query(server: &Server, params: &Value) -> Result<Value, RpcError> {
    let received = server.context.now();
    let mut results = Vec::new();
    for key in params["commands"].as_array().into_iter().flatten() {
        let session = key["sessionId"].as_str();
        let command = key["commandId"].as_str().unwrap_or_default();
        let known = server
            .remembered
            .lock_unpoisoned()
            .get(&inbox::key(session, command))
            .cloned();
        let known = match (known, session) {
            (Some(ack), _) => Some(ack),
            (None, Some(session)) => server
                .kernel
                .queries()
                .records(&SessionId::new(session), inbox::COMMAND_FACT)
                .await
                .ok()
                .and_then(|records| inbox::find_fact(&records, command)),
            (None, None) => None,
        };
        results.push(json!({
            "key": {"sessionId": session, "commandId": command},
            "result": known.map_or(json!("unknown"), |ack| ack.to_value()),
        }));
    }
    let mut reply = json!({"results": results});
    if params["clock"] == true {
        reply["clock"] = json!({
            "instanceId": server.instance,
            "receivedAt": received,
            "sentAt": server.context.now(),
        });
    }
    Ok(reply)
}
