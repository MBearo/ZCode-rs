//! 任务清单的改动与结果文字（agents.md §6）：不读对照数据的用例。按剧本重放的在 `edit_parity_tests.rs`
//! （docs/specs/rust-cli/export.md §5）。

use rust_cli_kernel_api::agents::TaskList;
use serde_json::json;

use super::{create, list_text, update};

#[test]
fn update_reports_only_changed_fields_in_order() {
    let mut list = TaskList::default();
    create(
        &mut list,
        serde_json::from_value(json!({"subject": "A", "description": "a"})).unwrap(),
    );
    let input = json!({
        "taskId": "1",
        "subject": "A",
        "description": "b",
        "status": "in_progress",
        "owner": "me",
        "metadata": {"k": 1}
    });
    let (text, changed) = update(&mut list, serde_json::from_value(input).unwrap());
    assert!(changed);
    assert_eq!(text, "Updated task #1 description, owner, metadata, status");
    let (text, changed) = update(
        &mut list,
        serde_json::from_value(json!({"taskId": "1", "metadata": {"k": null}})).unwrap(),
    );
    assert!(changed);
    assert_eq!(text, "Updated task #1 metadata");
    assert_eq!(
        list.tasks[0].metadata.as_ref().map(serde_json::Map::len),
        Some(0)
    );
    let (text, changed) = update(
        &mut list,
        serde_json::from_value(json!({"taskId": "7", "status": "completed"})).unwrap(),
    );
    assert_eq!((text.as_str(), changed), ("Task not found", false));
    assert_eq!(list_text(&TaskList::default()), "No tasks found");
}
