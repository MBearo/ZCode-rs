//! 取消登录（platform.md §5.6「取消」，TUI 的 esc）：取消是一条消息，轮询在下一次检查时停下，不写任何文件。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::LoginError;
use crate::fake_server_tests::{Captured, Harness, NOW_MS, Reply, Server};
use crate::test_support::read_text;
use crate::token::Family;

/// 授权一直没有完成的服务；返回轮询次数的计数。
async fn pending_service() -> (Server, Arc<AtomicUsize>) {
    let polls = Arc::new(AtomicUsize::new(0));
    let counted = polls.clone();
    let server = Server::start(move |request: &Captured| match request.path.as_str() {
        "/api/v1/oauth/cli/init" => Reply::json(
            200,
            &json!({"code": 0, "data": {
                "flow_id": "flow-1",
                "authorize_url": "https://chat.example.com/oauth?flow=1",
                "expires_at": NOW_MS / 1000 + 600,
                "poll_interval_sec": 1,
            }}),
        ),
        _ => {
            counted.fetch_add(1, Ordering::SeqCst);
            Reply::json(200, &json!({"code": 0, "data": {"status": "pending"}}))
        }
    })
    .await;
    (server, polls)
}

#[tokio::test]
async fn cancelling_stops_polling_and_writes_nothing() {
    let (server, polls) = pending_service().await;
    let harness = Harness::new(&server.url()).await;
    let login = harness.platform.begin_login(Family::Zai).await.unwrap();
    let cancel = CancellationToken::new();
    let canceller = async {
        // 等第一次轮询之后再取消：取消落在两次轮询之间的等待里。
        while polls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        cancel.cancel();
    };
    let (result, ()) = tokio::join!(login.complete_until(&cancel), canceller);
    assert_eq!(result.unwrap_err(), LoginError::Cancelled);
    assert_eq!(LoginError::Cancelled.to_string(), "Login cancelled.");
    assert_eq!(polls.load(Ordering::SeqCst), 1, "no poll after the cancel");
    assert!(
        read_text(harness.paths.credentials().as_path())
            .await
            .is_none()
    );
    assert!(
        read_text(harness.paths.provider_config().as_path())
            .await
            .is_none()
    );
}

#[tokio::test]
async fn an_already_cancelled_login_does_not_poll() {
    let (server, polls) = pending_service().await;
    let harness = Harness::new(&server.url()).await;
    let login = harness
        .platform
        .begin_login(Family::Bigmodel)
        .await
        .unwrap();
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        login.complete_until(&cancel).await.unwrap_err(),
        LoginError::Cancelled
    );
    assert_eq!(polls.load(Ordering::SeqCst), 0);
}

/// 轮询请求挂住时也能取消（修复线 F58，照 Node 的 `waitWithAbort`：请求与取消信号竞争）。以前取消只在两次轮询之间的
/// 等待里检查，请求没有单次超时，挂住时 esc 与登录期限都不起作用。
#[tokio::test]
async fn cancelling_interrupts_a_hanging_poll() {
    let polls = Arc::new(AtomicUsize::new(0));
    let counted = polls.clone();
    let server = Server::start(move |request: &Captured| match request.path.as_str() {
        "/api/v1/oauth/cli/init" => Reply::json(
            200,
            &json!({"code": 0, "data": {
                "flow_id": "flow-1",
                "authorize_url": "https://chat.example.com/oauth?flow=1",
                "expires_at": NOW_MS / 1000 + 600,
                "poll_interval_sec": 1,
            }}),
        ),
        _ => {
            counted.fetch_add(1, Ordering::SeqCst);
            Reply::json(200, &json!({"code": 0, "data": {"status": "pending"}})).after(60_000)
        }
    })
    .await;
    let harness = Harness::new(&server.url()).await;
    let login = harness.platform.begin_login(Family::Zai).await.unwrap();
    let cancel = CancellationToken::new();
    let canceller = async {
        while polls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        cancel.cancel();
    };
    let started = std::time::Instant::now();
    let (result, ()) = tokio::join!(login.complete_until(&cancel), canceller);
    assert_eq!(result.unwrap_err(), LoginError::Cancelled);
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
}
