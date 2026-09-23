use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TodoItem {
    pub content: String,
    pub status: Status,
    pub priority: Priority,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pending,
    InProgress,
    Completed,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    High,
    Medium,
    Low,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Read {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Write {
    todos: Vec<TodoItem>,
}
pub fn parse(name: &str, args: Value) -> Result<Option<Vec<TodoItem>>> {
    if name == "TodoRead" {
        serde_json::from_value::<Read>(args)?;
        return Ok(None);
    }
    let Write { todos } = serde_json::from_value(args)?;
    validate(&todos)?;
    Ok(Some(todos))
}
pub fn validate(todos: &[TodoItem]) -> Result<()> {
    ensure!(
        todos.iter().all(|t| !t.content.is_empty()),
        "Todo content must not be empty"
    );
    Ok(())
}
pub fn result(old: &[TodoItem], next: Option<&[TodoItem]>) -> Value {
    match next {
        None => json!({"todos":old}),
        Some(todos) => json!({"oldTodos":old,"todos":todos,"summary":{
            "total":todos.len(),
            "pending":todos.iter().filter(|t|matches!(t.status,Status::Pending)).count(),
            "inProgress":todos.iter().filter(|t|matches!(t.status,Status::InProgress)).count(),
            "completed":todos.iter().filter(|t|matches!(t.status,Status::Completed)).count(),
        }}),
    }
}
pub fn plan(todos: &[TodoItem], updated_at: u64) -> Value {
    let items = todos.iter().filter_map(|t| {
        let title=t.content.trim();
        (!title.is_empty()).then(|| json!({"id":title,"content":title,"status":match t.status {
            Status::Pending=>"pending",Status::InProgress=>"inProgress",Status::Completed=>"completed"
        }}))
    }).collect::<Vec<_>>();
    if items.is_empty() {
        Value::Null
    } else {
        json!({"items":items,"updatedAt":updated_at})
    }
}
pub fn model_content(value: &Value) -> String {
    let mut text = value.to_string();
    if text.len() > 100_000 {
        let suffix = format!(
            "\n\n[Tool output truncated by resultBudget: originalBytes={}, maxModelBytes=100000, strategy=truncate]",
            text.len()
        );
        let mut end = 100_000 - suffix.len();
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str(&suffix);
    }
    text
}
pub fn should_remind(messages: &[Value]) -> bool {
    let mut turns = 0;
    for m in messages.iter().rev() {
        if m["_zcode_source"] == "todo_reminder" {
            return false;
        }
        if m["role"] != "assistant" {
            continue;
        }
        if m["tool_calls"]
            .as_array()
            .is_some_and(|calls| calls.iter().any(|c| c["function"]["name"] == "TodoWrite"))
        {
            return false;
        }
        turns += 1;
        if turns >= 10 {
            return true;
        }
    }
    false
}
pub fn reminder(todos: &[TodoItem]) -> String {
    let mut text = String::from(
        "The TodoWrite tool hasn't been used recently. If you're working on tasks that would benefit from tracking progress, consider using the TodoWrite tool to track progress. Also consider cleaning up the todo list if has become stale and no longer matches what you are working on. Only use it if it's relevant to the current work. This is just a gentle reminder - ignore if not applicable.",
    );
    if !todos.is_empty() {
        text.push_str("\n\nHere are the existing contents of your todo list:\n\n[");
        for (i, todo) in todos.iter().enumerate() {
            if i > 0 {
                text.push('\n');
            }
            let status = match todo.status {
                Status::Pending => "pending",
                Status::InProgress => "in_progress",
                Status::Completed => "completed",
            };
            text.push_str(&format!("{}. [{}] {}", i + 1, status, todo.content));
        }
        text.push(']');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reminder_thresholds_ignore_tool_results_and_reset_on_write_or_reminder() {
        let assistant = json!({"role":"assistant","content":"done"});
        let mut messages = vec![assistant.clone(); 9];
        assert!(!should_remind(&messages));
        messages.push(assistant.clone());
        assert!(should_remind(&messages));
        messages.push(json!({"role":"user","_zcode_source":"todo_reminder"}));
        messages.extend(vec![assistant.clone(); 9]);
        assert!(!should_remind(&messages));
        messages.push(json!({"role":"tool","content":"done"}));
        assert!(!should_remind(&messages));
        messages.push(assistant.clone());
        assert!(should_remind(&messages));
        messages.push(json!({"role":"assistant","tool_calls":[{"function":{"name":"TodoWrite"}}]}));
        messages.extend(vec![assistant.clone(); 9]);
        assert!(!should_remind(&messages));
        messages.push(assistant);
        assert!(should_remind(&messages));
    }
}
