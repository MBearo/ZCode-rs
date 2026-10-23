//! 任务清单：文件是事实源，写后投影到 `todo` 表（Node `updateTodos` 的写法）。
use rust_cli_kernel_api::agents::{TaskList, TaskListStore, TaskRecord, TaskStatus};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::store::SessionStore;
use serde_json::json;

use crate::tests::{TestStore, meta};

fn task(id: &str, subject: &str, status: TaskStatus) -> TaskRecord {
    TaskRecord {
        id: id.to_owned(),
        subject: subject.to_owned(),
        description: String::new(),
        active_form: None,
        owner: None,
        status,
        blocks: Vec::new(),
        blocked_by: Vec::new(),
        metadata: None,
    }
}

#[tokio::test]
async fn saving_writes_the_file_and_projects_the_todo_rows() {
    let test = TestStore::new();
    let session = SessionId::new("sess_1");
    test.store
        .create(meta("sess_1", "/work", ""))
        .await
        .unwrap();
    assert_eq!(
        TaskListStore::load(&test.store, &session).await.unwrap(),
        TaskList::default()
    );
    let list = TaskList {
        high_water_mark: 10,
        tasks: vec![
            task("10", "Ship it", TaskStatus::Pending),
            task("2", "Write tests", TaskStatus::InProgress),
        ],
    };
    test.source.at(5000);
    TaskListStore::save(&test.store, &session, &list)
        .await
        .unwrap();
    assert_eq!(
        TaskListStore::load(&test.store, &session).await.unwrap(),
        list
    );
    let file = test.dir.path().join("tasks/sess_1.json");
    let bytes = rust_cli_fs::read_optional(&file).await.unwrap().unwrap();
    let saved: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(saved["highWaterMark"], 10);
    // 按 ID 的数值升序投影；priority 是占位；会话的 time_updated 跟着前移。
    assert_eq!(
        test.rows("select session_id, content, status, priority, position, time_created, time_updated from todo order by position"),
        vec![
            vec![json!("sess_1"), json!("Write tests"), json!("in_progress"), json!("medium"), json!(0), json!(5000), json!(5000)],
            vec![json!("sess_1"), json!("Ship it"), json!("pending"), json!("medium"), json!(1), json!(5000), json!(5000)],
        ]
    );
    assert_eq!(
        test.rows("select time_updated from session"),
        vec![vec![json!(5000)]]
    );
    // 整体替换：删掉的任务不再出现在投影里。
    let shorter = TaskList {
        high_water_mark: 10,
        tasks: vec![task("10", "Ship it", TaskStatus::Completed)],
    };
    TaskListStore::save(&test.store, &session, &shorter)
        .await
        .unwrap();
    assert_eq!(
        test.rows("select content, status, position from todo"),
        vec![vec![json!("Ship it"), json!("completed"), json!(0)]]
    );
}

/// 续写 Node 会话（修复线 F2）：没有清单文件时从 Node 的 todo 导入；追加任务后 Node 的 todo（含优先级）还在。
#[tokio::test]
async fn a_node_todo_list_is_imported_and_kept() {
    let test = TestStore::new();
    let session = SessionId::new("sess_1");
    test.store
        .create(meta("sess_1", "/work", ""))
        .await
        .unwrap();
    test.connection()
        .execute_batch(
            "insert into todo (session_id, content, status, priority, position, time_created, time_updated) values
             ('sess_1', 'Second', 'pending', 'medium', 1, 1, 1),
             ('sess_1', 'First', 'in_progress', 'high', 0, 1, 1)",
        )
        .unwrap();
    let mut list = TaskListStore::load(&test.store, &session).await.unwrap();
    assert_eq!(list.high_water_mark, 2);
    assert_eq!(
        list.tasks
            .iter()
            .map(|t| (t.id.as_str(), t.subject.as_str(), t.status))
            .collect::<Vec<_>>(),
        vec![
            ("1", "First", TaskStatus::InProgress),
            ("2", "Second", TaskStatus::Pending)
        ]
    );
    list.high_water_mark = 3;
    list.tasks.push(task("3", "Third", TaskStatus::Pending));
    TaskListStore::save(&test.store, &session, &list)
        .await
        .unwrap();
    assert_eq!(
        test.rows("select content, status, priority, position from todo order by position"),
        vec![
            vec![
                json!("First"),
                json!("in_progress"),
                json!("high"),
                json!(0)
            ],
            vec![json!("Second"), json!("pending"), json!("medium"), json!(1)],
            vec![json!("Third"), json!("pending"), json!("medium"), json!(2)],
        ]
    );
}
