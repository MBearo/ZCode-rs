use std::sync::Arc;

use rust_cli_base::TaskScope;
use rust_cli_kernel_api::agents::{TaskList, TaskListStore};
use rust_cli_kernel_api::ids::SessionId;

use super::{MemoryTaskLists, TaskBoard, task_tools_enabled};

#[test]
fn task_tools_follow_the_model_gate() {
    // 有模型卡片的模型默认关闭；旧模型与不认识的模型打开。
    assert!(!task_tools_enabled("claude-opus-5-5", false));
    assert!(!task_tools_enabled("claude-opus-5-5[1m]", false));
    assert!(task_tools_enabled("claude-opus-5-5", true));
    assert!(task_tools_enabled("glm-5", false));
}

#[tokio::test]
async fn the_board_writes_only_when_something_changed() {
    let scope = TaskScope::root("test", |_| {});
    let store = Arc::new(MemoryTaskLists::default());
    let board = TaskBoard::new(store.clone(), &scope.spawner());
    let list = SessionId::new("sess_1");
    let (text, written) = board
        .update(&list, Box::new(|_| ("unchanged".to_owned(), false)))
        .await
        .unwrap();
    assert_eq!((text.as_str(), written), ("unchanged", None));
    let (_, written) = board
        .update(
            &list,
            Box::new(|tasks: &mut TaskList| {
                tasks.high_water_mark = 3;
                ("changed".to_owned(), true)
            }),
        )
        .await
        .unwrap();
    assert_eq!(written.map(|tasks| tasks.high_water_mark), Some(3));
    assert_eq!(store.load(&list).await.unwrap().high_water_mark, 3);
    assert_eq!(board.read(&list).await.unwrap().high_water_mark, 3);
}
