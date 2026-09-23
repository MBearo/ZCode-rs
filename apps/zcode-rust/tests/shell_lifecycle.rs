#![cfg(unix)]

use serde_json::json;
use std::{path::Path, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
use zcode_rust::adapters::tools::WorkspaceTools;

async fn pid(path: &Path) -> i32 {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(text) = tokio::fs::read_to_string(path).await
                && let Ok(pid) = text.trim().parse::<i32>()
            {
                return pid;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}
struct Cleanup(Vec<i32>);
impl Drop for Cleanup {
    fn drop(&mut self) {
        for pid in &self.0 {
            // 失败用例也回收自己创建且仍在独立组内的测试进程，不遗留 30 秒工作进程。
            unsafe {
                libc::kill(-*pid, libc::SIGKILL);
            }
        }
    }
}
async fn stopped(pid: i32) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while unsafe { libc::kill(pid, 0) } == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("owned descendant is still alive");
}

#[tokio::test]
async fn cancellation_honors_term_cleanup_and_retains_output() {
    let root = tempfile::tempdir().unwrap();
    let tools = Arc::new(WorkspaceTools::new(
        root.path().into(),
        root.path().join("artifacts"),
    ));
    let cancel = CancellationToken::new();
    let job = {
        let tools = tools.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            tools.call("s", "Bash", &json!({"command":"trap 'printf cleaned > cleanup; exit 0' TERM; echo $$ > pid; printf before-cancel; while :; do sleep 0.05; done"}), &cancel).await
        })
    };
    let leader = pid(&root.path().join("pid")).await;
    let _cleanup = Cleanup(vec![leader]);
    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(3), job)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(result.data["status"], "cancelled");
    assert!(
        result.data["stdout"]
            .as_str()
            .unwrap()
            .contains("before-cancel")
    );
    assert_eq!(
        tokio::fs::read_to_string(root.path().join("cleanup"))
            .await
            .unwrap(),
        "cleaned"
    );
    stopped(leader).await;
}

#[tokio::test]
async fn cancellation_reaps_term_ignoring_job_control_descendants() {
    let root = tempfile::tempdir().unwrap();
    let tools = Arc::new(WorkspaceTools::new(
        root.path().into(),
        root.path().join("artifacts"),
    ));
    let cancel = CancellationToken::new();
    let job = {
        let tools = tools.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            tools.call("s", "Bash", &json!({"command":"set -m; /bin/bash -c 'trap \"\" TERM; echo $$ > child.pid; while :; do sleep 0.05; done' & echo $$ > root.pid; wait"}), &cancel).await
        })
    };
    let leader = pid(&root.path().join("root.pid")).await;
    let child = pid(&root.path().join("child.pid")).await;
    let _cleanup = Cleanup(vec![leader, child]);
    assert_ne!(unsafe { libc::getpgid(child) }, leader);
    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(3), job).await;
    let result = result
        .expect("job-control worker survived cancellation")
        .unwrap()
        .unwrap();
    assert_eq!(result.data["status"], "cancelled");
    stopped(child).await;
    stopped(leader).await;
}

#[tokio::test]
async fn timeout_reaps_cross_group_workers_without_explicit_cancellation() {
    let root = tempfile::tempdir().unwrap();
    let tools = Arc::new(WorkspaceTools::new(
        root.path().into(),
        root.path().join("artifacts"),
    ));
    let job = tokio::spawn({
        let tools = tools.clone();
        async move {
            tools.call("s", "Bash", &json!({"command":"set -m; /bin/bash -c 'trap \"\" TERM; echo $$ > child.pid; while :; do sleep 0.05; done' & echo $$ > root.pid; wait", "timeout":1000}), &CancellationToken::new()).await
        }
    });
    let leader = pid(&root.path().join("root.pid")).await;
    let child = pid(&root.path().join("child.pid")).await;
    let _cleanup = Cleanup(vec![leader, child]);
    let result = tokio::time::timeout(Duration::from_secs(4), job)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(result.data["status"], "timed_out");
    assert_eq!(result.data["timedOut"], true);
    assert_eq!(result.data["cancelled"], false);
    stopped(leader).await;
    stopped(child).await;
}

#[tokio::test]
async fn normal_leader_exit_reaps_workers_that_hold_output_pipes() {
    let root = tempfile::tempdir().unwrap();
    let tools = Arc::new(WorkspaceTools::new(
        root.path().into(),
        root.path().join("artifacts"),
    ));
    let job = tokio::spawn({
        let tools = tools.clone();
        async move {
            tools.call("s", "Bash", &json!({"command":"sleep 30 & echo $! > child.pid; echo $$ > root.pid; printf leader-finished; exit 0"}), &CancellationToken::new()).await
        }
    });
    let leader = pid(&root.path().join("root.pid")).await;
    let child = pid(&root.path().join("child.pid")).await;
    let _cleanup = Cleanup(vec![leader]);
    let result = tokio::time::timeout(Duration::from_secs(3), job)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(result.data["status"], "completed");
    assert_eq!(result.data["stdout"], "leader-finished");
    stopped(leader).await;
    stopped(child).await;
}
