use std::sync::Arc;

use rust_cli_base::TaskScope;
use rust_cli_base::models::{DEEPSEEK_V4_PRO, GLM_5_3, HAIKU_3_5_DATED, OPUS_5_5};
use rust_cli_kernel_api::agents::{TaskList, TaskListStore};
use rust_cli_kernel_api::ids::SessionId;

use super::{MemoryTaskLists, TaskBoard, task_tools_enabled};

/// 有卡片形状的模型（Anthropic 的有卡片的模型、按 opus 处理的 GLM）缺省关闭；没有卡片的模型（旧 Anthropic 模型、
/// DeepSeek）打开；显式打开时都打开（agents.md §7.1，工作线 F121 删掉了不起作用的旧模型清单）。
#[test]
fn task_tools_follow_the_model_gate() {
    for shaped in [OPUS_5_5, GLM_5_3] {
        assert!(!task_tools_enabled(shaped, false), "{shaped}");
        assert!(
            !task_tools_enabled(&format!("{shaped}[1m]"), false),
            "{shaped}"
        );
        assert!(task_tools_enabled(shaped, true), "{shaped}");
    }
    for uncarded in [HAIKU_3_5_DATED, DEEPSEEK_V4_PRO] {
        assert!(task_tools_enabled(uncarded, false), "{uncarded}");
    }
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
