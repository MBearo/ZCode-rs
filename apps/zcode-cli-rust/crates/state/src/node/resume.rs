//! The persisted facts a cold resume reads (Node `resumeFromStore` and the
//! bootstrap model selection restore). Spec rust-m11-node-storage §6.1.
use super::codecs::{MODEL_SELECTION_ENTRY, parse_model_selection};
use super::cold::{self, History};
use super::entries;
use super::sessions::{self, SessionRow};
use super::targets::{self, Target};
use super::todos::{self, Todo};
use anyhow::Result;
use rusqlite::Connection;
use serde_json::{Map, Value, json};
use zcode_cli_domain::node_history::{self, Branch, Record};
use zcode_cli_domain::node_rows::{self, Cold};

const EXECUTION_STATE_ENTRY: &str = "runtime/execution_state";
const PERMISSION_FULL_ACCESS_ENTRY: &str = "runtime/permission_full_access";
/// Node `executionPermissionModeSchema`.
const EXECUTION_MODES: [&str; 4] = ["build", "edit", "yolo", "auto"];

/// A stored session as a restarted runtime restores it.
#[derive(Clone, Debug, PartialEq)]
pub struct Resume {
    pub session: SessionRow,
    pub model_selection: Option<Value>,
    /// `{mode, planEnabled}`; `None` keeps the runtime's defaults.
    pub execution: Option<Value>,
    pub permission_grant: Option<String>,
    pub todos: Vec<Todo>,
    pub target: Option<Target>,
    pub turn_number: usize,
    pub latest_message: Option<String>,
    pub latest_assistant: Option<String>,
    pub latest_assistant_turn: Option<String>,
    pub last_assistant_completed: Option<Value>,
    pub history: History,
    pub conversation: Cold,
    /// The workspace checkpoints (spec §5.5).
    pub checkpoints: Vec<crate::domain::file_checkpoint::ImportedCheckpoint>,
}

fn last_entry(conn: &Connection, session: &str, kind: &str) -> Result<Option<Value>> {
    Ok(entries::list(conn, session, Some(kind))?
        .pop()
        .map(|entry| entry.data))
}

/// Node `readSessionModelSelection`: the complete selection, else only the
/// model identity when the options are malformed.
fn model_selection(data: Option<Value>) -> Option<Value> {
    let data = data?;
    parse_model_selection(&data).or_else(|| {
        let object = data.as_object()?;
        let mut identity = Map::new();
        for key in ["providerId", "modelId"] {
            identity.insert(key.into(), object.get(key).cloned().unwrap_or(Value::Null));
        }
        parse_model_selection(&Value::Object(identity))
    })
}

/// Node `resolveExecutionState({ mode })` with the default current state.
fn resolve_execution(mode: &Value) -> Value {
    let valid = mode.as_str().filter(|m| EXECUTION_MODES.contains(m));
    json!({
        "mode": valid.unwrap_or("build"),
        "planEnabled": valid.is_none() && mode == "plan",
    })
}

/// Node `executionStateSchema.safeParse(...).data`.
fn saved_execution(data: Option<Value>) -> Option<Value> {
    let data = data?;
    let mode = data["mode"]
        .as_str()
        .filter(|m| EXECUTION_MODES.contains(m))?;
    let plan = data["planEnabled"].as_bool()?;
    data.is_object()
        .then(|| json!({"mode": mode, "planEnabled": plan}))
}

fn strict<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Map<String, Value>> {
    let object = value.as_object()?;
    object
        .keys()
        .all(|k| keys.contains(&k.as_str()))
        .then_some(object)
}

fn nonempty(value: Option<&Value>) -> Option<&str> {
    value?.as_str().filter(|s| !s.is_empty())
}

/// `z.coerce.date()`: whatever `new Date(value)` turns into a valid date.
fn coercible_date(value: Option<&Value>) -> bool {
    match value {
        None => false,
        Some(Value::Null | Value::Bool(_)) => true,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|ms| ms.abs() <= 8.64e15),
        Some(Value::String(s)) => {
            chrono::DateTime::parse_from_rfc3339(s).is_ok()
                || chrono::DateTime::parse_from_rfc2822(s).is_ok()
                || chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f").is_ok()
                || chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
        }
        Some(_) => false,
    }
}

/// Node `permissionFullAccessReceiptSchema` (strict, with the identity
/// refinement): the receipt's interaction and owning session.
fn receipt(data: &Value) -> Option<(&str, &str)> {
    let top = strict(data, &["interactionId", "event"])?;
    let interaction = nonempty(top.get("interactionId"))?;
    let event = strict(
        top.get("event")?,
        &[
            "id",
            "sessionId",
            "traceId",
            "turnId",
            "type",
            "timestamp",
            "sequenceNumber",
            "payload",
        ],
    )?;
    let session = nonempty(event.get("sessionId"))?;
    nonempty(event.get("id"))?;
    nonempty(event.get("traceId"))?;
    if event.get("turnId").is_some_and(|t| !t.is_string())
        || event.get("type")? != "session_mode_changed"
        || !coercible_date(event.get("timestamp"))
        || !event
            .get("sequenceNumber")?
            .as_f64()
            .is_some_and(|n| n >= 0.0 && n.fract() == 0.0)
    {
        return None;
    }
    let payload = strict(
        event.get("payload")?,
        &[
            "mode",
            "planEnabled",
            "previousMode",
            "previousPlanEnabled",
            "source",
            "permissionGrant",
        ],
    )?;
    let modes = ["build", "edit", "yolo", "auto", "plan"];
    if payload.get("mode")? != "yolo"
        || !payload.get("planEnabled")?.is_boolean()
        || !payload
            .get("previousMode")?
            .as_str()
            .is_some_and(|m| modes.contains(&m))
        || !payload.get("previousPlanEnabled")?.is_boolean()
        || payload.get("source")? != "command"
    {
        return None;
    }
    let grant = strict(
        payload.get("permissionGrant")?,
        &["interactionId", "queueItemIds"],
    )?;
    let granted = nonempty(grant.get("interactionId"))?;
    let queue_ok = grant
        .get("queueItemIds")?
        .as_array()?
        .iter()
        .all(|id| nonempty(Some(id)).is_some());
    (queue_ok && granted == interaction).then_some((interaction, session))
}

/// Node `restorePermissionGrantMarker`: the last receipt, when valid and ours.
fn permission_grant(data: Option<Value>, session: &str) -> Option<String> {
    let data = data?;
    let (interaction, owner) = receipt(&data)?;
    (owner == session).then(|| interaction.to_owned())
}

/// The cold-loaded session, or `None` when it is missing or archived
/// (Node `SessionNotFound`). `context_window` is the selected model's window.
pub fn resume(
    conn: &Connection,
    id: &str,
    artifacts: &dyn Fn(&str) -> Option<String>,
    context_window: Option<f64>,
) -> Result<Option<Resume>> {
    let Some(session) = sessions::get(conn, id)?.filter(|s| s.time_archived.is_none()) else {
        return Ok(None);
    };
    let execution = saved_execution(last_entry(conn, id, EXECUTION_STATE_ENTRY)?).or_else(|| {
        session
            .permission
            .as_ref()
            .and_then(|p| p.get("mode"))
            .map(resolve_execution)
    });
    let branch = Branch::from_revert(session.revert.as_ref());
    let all = cold::records(conn, id)?;
    let active = node_history::active_messages(&all, &branch, true);
    let turn_number = active
        .iter()
        .filter(|m| {
            m.info["role"] == "user" && !node_history::branch::truthy(m.info.get("summary"))
        })
        .count();
    let history = cold::history_of(&active, artifacts);
    drop(active);
    // 最新消息锚点不含压缩保留段：保留段不是压缩后时间线的最新位置。
    let timeline = node_history::active_messages(&all, &branch, false);
    let latest = timeline
        .iter()
        .rev()
        .find(|m| matches!(m.info["role"].as_str(), Some("user" | "assistant")))
        .map(|m| m.id().to_owned());
    let assistant = timeline
        .iter()
        .rev()
        .find(|m| m.info["role"] == "assistant")
        .map(|m| Record::clone(m));
    drop(timeline);
    let m = cold::materialization_of(conn, id, all, &branch)?;
    let sources = node_rows::Sources {
        context_window,
        goal_entries: &m.goal_entries,
        ..Default::default()
    };
    let events = node_rows::cold_events(&m.messages, sources, m.target.as_ref());
    let assistant = assistant.as_ref();
    Ok(Some(Resume {
        model_selection: model_selection(last_entry(conn, id, MODEL_SELECTION_ENTRY)?),
        execution,
        permission_grant: permission_grant(last_entry(conn, id, PERMISSION_FULL_ACCESS_ENTRY)?, id),
        todos: todos::read(conn, id)?,
        target: targets::read(conn, id)?,
        turn_number,
        latest_message: latest,
        latest_assistant: assistant.map(|m| m.id().to_owned()),
        latest_assistant_turn: assistant
            .and_then(|m| m.info["anchor"]["turnId"].as_str())
            .map(str::to_owned),
        last_assistant_completed: assistant.and_then(|m| m.info["time"].get("completed").cloned()),
        history,
        checkpoints: super::checkpoints::read(conn, id, artifacts)?,
        conversation: node_rows::replay(id, &events),
        session,
    }))
}
