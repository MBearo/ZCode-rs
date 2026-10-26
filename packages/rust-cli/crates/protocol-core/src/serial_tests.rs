use std::sync::{Arc, Mutex};
use std::time::Duration;

use rust_cli_base::TaskScope;

use super::*;

/// 工作按提交的顺序跑完一件再跑下一件（前一件在等也一样）。
#[tokio::test]
async fn jobs_run_one_after_another_in_order() {
    let scope = TaskScope::root("test", |_| {});
    let serial = Serial::new(&scope.spawner(), "serial");
    let seen = Arc::new(Mutex::new(Vec::new()));
    for (index, delay) in [(1, 30), (2, 0), (3, 10)] {
        let seen = seen.clone();
        serial.run(Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(delay)).await;
            seen.lock().unwrap().push(index);
        }));
    }
    let (done, wait) = tokio::sync::oneshot::channel();
    serial.run(Box::pin(async move {
        let _ = done.send(());
    }));
    wait.await.unwrap();
    assert_eq!(*seen.lock().unwrap(), vec![1, 2, 3]);
}
