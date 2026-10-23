//! `.zcode/scheduled_tasks.json` 的解析与写出（docs/specs/rust-cli/tools-more.md §16.1）。
//!
//! 文件形状 `{"tasks": [...]}`，`JSON.stringify(_, null, 2)` 加换行。读时跳过字段类型不对的项（只记
//! debug 日志）；cron 表达式是否合法由内核判断（D30）。

use serde_json::{Map, Value, json};

/// 文件里的一项，保留文件里的全部字段。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FileTask {
    pub(crate) id: String,
    pub(crate) cron: String,
    pub(crate) prompt: String,
    pub(crate) created_at: u64,
    pub(crate) last_fired_at: Option<u64>,
    pub(crate) recurring: bool,
    pub(crate) permanent: bool,
    pub(crate) created_by_session: Option<String>,
    pub(crate) created_by_pid: Option<u32>,
    pub(crate) created_by_proc_start: Option<String>,
    pub(crate) created_in_project: Option<String>,
}

/// JS 的真值判断（`recurring` 为真值时才写 `recurring: true`）。
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_f64().is_some_and(|n| n != 0.0 && !n.is_nan()),
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(_) | Value::Object(_)) => true,
    }
}

/// 毫秒时间戳：JS 的 number；小数向下取整，负数当作 0。
fn millis(value: &Value) -> Option<u64> {
    let number = value.as_f64()?;
    number.is_finite().then(|| number.max(0.0) as u64)
}

fn text(item: &Map<String, Value>, key: &str) -> Option<String> {
    item.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn task(item: &Value) -> Option<FileTask> {
    let item = item.as_object()?;
    Some(FileTask {
        id: text(item, "id")?,
        cron: text(item, "cron")?,
        prompt: text(item, "prompt")?,
        created_at: millis(item.get("createdAt")?)?,
        last_fired_at: item.get("lastFiredAt").and_then(millis),
        recurring: truthy(item.get("recurring")),
        permanent: truthy(item.get("permanent")),
        created_by_session: text(item, "createdBySessionId"),
        created_by_pid: item
            .get("createdByPid")
            .and_then(Value::as_f64)
            .filter(|pid| pid.is_finite() && *pid >= 0.0 && *pid <= f64::from(u32::MAX))
            .map(|pid| pid as u32),
        created_by_proc_start: text(item, "createdByProcStart"),
        created_in_project: text(item, "createdInProject"),
    })
}

/// 文件内容 → 任务；不是 JSON、没有 `tasks` 数组时为空。
pub(crate) fn parse(raw: &str) -> Vec<FileTask> {
    let Ok(Value::Object(file)) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };
    let Some(Value::Array(items)) = file.get("tasks") else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let parsed = task(item);
            if parsed.is_none() {
                tracing::debug!(%item, "skipping a malformed scheduled task");
            }
            parsed
        })
        .collect()
}

/// 任务 → 文件内容（读后的键序）。
pub(crate) fn render(tasks: &[FileTask]) -> String {
    let items: Vec<Value> = tasks
        .iter()
        .map(|task| {
            let mut item = Map::new();
            item.insert("id".to_owned(), json!(task.id));
            item.insert("cron".to_owned(), json!(task.cron));
            item.insert("prompt".to_owned(), json!(task.prompt));
            item.insert("createdAt".to_owned(), json!(task.created_at));
            if let Some(at) = task.last_fired_at {
                item.insert("lastFiredAt".to_owned(), json!(at));
            }
            if task.recurring {
                item.insert("recurring".to_owned(), json!(true));
            }
            if task.permanent {
                item.insert("permanent".to_owned(), json!(true));
            }
            if let Some(session) = &task.created_by_session {
                item.insert("createdBySessionId".to_owned(), json!(session));
            }
            if let Some(pid) = task.created_by_pid {
                item.insert("createdByPid".to_owned(), json!(pid));
            }
            if let Some(start) = &task.created_by_proc_start {
                item.insert("createdByProcStart".to_owned(), json!(start));
            }
            if let Some(project) = &task.created_in_project {
                item.insert("createdInProject".to_owned(), json!(project));
            }
            Value::Object(item)
        })
        .collect();
    let file = json!({ "tasks": items });
    let mut text = serde_json::to_string_pretty(&file).unwrap_or_else(|_| "{}".to_owned());
    text.push('\n');
    text
}

#[cfg(test)]
#[path = "cron_file_tests.rs"]
mod tests;
