//! 任务清单的修改与文本（各工具的 `call` 与结果文本，结果文本逐字照写）。
//! 纯函数，不做 IO。

use rust_cli_kernel_api::agents::{TaskList, TaskRecord, TaskStatus};
use serde::Deserialize;
use serde_json::{Map, Value};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateInput {
    subject: String,
    description: String,
    #[serde(default)]
    active_form: Option<String>,
    #[serde(default)]
    metadata: Option<Map<String, Value>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GetInput {
    pub(crate) task_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateInput {
    task_id: String,
    #[serde(default)]
    subject: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    active_form: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    add_blocks: Vec<String>,
    #[serde(default)]
    add_blocked_by: Vec<String>,
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    metadata: Option<Map<String, Value>>,
}

fn ids(ids: &[String]) -> String {
    ids.iter()
        .map(|id| format!("#{id}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `from` 挡住 `to`，两边都要存在。
fn block(list: &mut TaskList, from: &str, to: &str) {
    if !list.tasks.iter().any(|task| task.id == to)
        || !list.tasks.iter().any(|task| task.id == from)
    {
        return;
    }
    for task in &mut list.tasks {
        if task.id == from && !task.blocks.iter().any(|id| id == to) {
            task.blocks.push(to.to_owned());
        }
        if task.id == to && !task.blocked_by.iter().any(|id| id == from) {
            task.blocked_by.push(from.to_owned());
        }
    }
}

pub(crate) fn create(list: &mut TaskList, input: CreateInput) -> String {
    list.high_water_mark += 1;
    let id = list.high_water_mark.to_string();
    let text = format!("Task #{id} created successfully: {}", input.subject);
    list.tasks.push(TaskRecord {
        id,
        subject: input.subject,
        description: input.description,
        active_form: input.active_form,
        owner: None,
        status: TaskStatus::Pending,
        blocks: Vec::new(),
        blocked_by: Vec::new(),
        metadata: input.metadata,
    });
    text
}

pub(crate) fn get(list: &TaskList, id: &str) -> String {
    let Some(task) = list.tasks.iter().find(|task| task.id == id) else {
        return "Task not found".to_owned();
    };
    let mut lines = vec![
        format!("Task #{}: {}", task.id, task.subject),
        format!("Status: {}", task.status.as_str()),
        format!("Description: {}", task.description),
    ];
    if !task.blocked_by.is_empty() {
        lines.push(format!("Blocked by: {}", ids(&task.blocked_by)));
    }
    if !task.blocks.is_empty() {
        lines.push(format!("Blocks: {}", ids(&task.blocks)));
    }
    lines.join("\n")
}

pub(crate) fn list_text(list: &TaskList) -> String {
    let visible: Vec<&TaskRecord> = list
        .tasks
        .iter()
        .filter(|task| {
            !task
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.get("_internal"))
                .is_some_and(|value| !value.is_null() && value != &Value::Bool(false))
        })
        .collect();
    if visible.is_empty() {
        return "No tasks found".to_owned();
    }
    let completed: Vec<&str> = visible
        .iter()
        .filter(|task| task.status == TaskStatus::Completed)
        .map(|task| task.id.as_str())
        .collect();
    visible
        .iter()
        .map(|task| {
            let owner = task
                .owner
                .as_ref()
                .map(|owner| format!(" ({owner})"))
                .unwrap_or_default();
            let open: Vec<String> = task
                .blocked_by
                .iter()
                .filter(|id| !completed.contains(&id.as_str()))
                .cloned()
                .collect();
            let blocked = if open.is_empty() {
                String::new()
            } else {
                format!(" [blocked by {}]", ids(&open))
            };
            format!(
                "#{} [{}] {}{owner}{blocked}",
                task.id,
                task.status.as_str(),
                task.subject
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn status(value: &str) -> Option<TaskStatus> {
    match value {
        "pending" => Some(TaskStatus::Pending),
        "in_progress" => Some(TaskStatus::InProgress),
        "completed" => Some(TaskStatus::Completed),
        _ => None,
    }
}

/// TaskUpdate：只记真正变化的字段；`deleted` 直接删除（同时去掉别的任务对它的引用）。
pub(crate) fn update(list: &mut TaskList, input: UpdateInput) -> (String, bool) {
    let Some(index) = list.tasks.iter().position(|task| task.id == input.task_id) else {
        return ("Task not found".to_owned(), false);
    };
    let id = input.task_id.clone();
    if input.status.as_deref() == Some("deleted") {
        list.tasks.remove(index);
        for task in &mut list.tasks {
            task.blocks.retain(|other| other != &id);
            task.blocked_by.retain(|other| other != &id);
        }
        return (format!("Updated task #{id} deleted"), true);
    }
    let mut fields: Vec<&str> = Vec::new();
    let task = &mut list.tasks[index];
    if let Some(subject) = input.subject.filter(|subject| subject != &task.subject) {
        task.subject = subject;
        fields.push("subject");
    }
    if let Some(description) = input.description.filter(|value| value != &task.description) {
        task.description = description;
        fields.push("description");
    }
    if let Some(form) = input
        .active_form
        .filter(|form| task.active_form.as_ref() != Some(form))
    {
        task.active_form = Some(form);
        fields.push("activeForm");
    }
    if let Some(owner) = input
        .owner
        .filter(|owner| task.owner.as_ref() != Some(owner))
    {
        task.owner = Some(owner);
        fields.push("owner");
    }
    if let Some(changes) = input.metadata {
        let metadata = task.metadata.get_or_insert_with(Map::new);
        for (key, value) in changes {
            if value.is_null() {
                metadata.remove(&key);
            } else {
                metadata.insert(key, value);
            }
        }
        fields.push("metadata");
    }
    if let Some(next) = input
        .status
        .as_deref()
        .and_then(status)
        .filter(|next| *next != task.status)
    {
        task.status = next;
        fields.push("status");
    }
    let blocks: Vec<String> = input
        .add_blocks
        .into_iter()
        .filter(|other| !list.tasks[index].blocks.contains(other))
        .collect();
    for other in &blocks {
        block(list, &id, other);
    }
    if !blocks.is_empty() {
        fields.push("blocks");
    }
    let blocked_by: Vec<String> = input
        .add_blocked_by
        .into_iter()
        .filter(|other| !list.tasks[index].blocked_by.contains(other))
        .collect();
    for other in &blocked_by {
        block(list, other, &id);
    }
    if !blocked_by.is_empty() {
        fields.push("blockedBy");
    }
    let changed = !fields.is_empty();
    (format!("Updated task #{id} {}", fields.join(", ")), changed)
}

#[cfg(test)]
#[path = "edit_tests.rs"]
mod tests;

#[cfg(test)]
#[cfg(feature = "parity")]
#[rustfmt::skip]
#[path = "edit_parity_tests.rs"]
mod parity_tests;
