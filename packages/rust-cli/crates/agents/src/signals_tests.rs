use futures_util::StreamExt;
use rust_cli_kernel_api::agents::AgentSignal;
use rust_cli_kernel_api::ids::{AgentId, SessionId};
use serde_json::json;

use super::Hub;

#[tokio::test]
async fn unacked_notices_are_delivered_again_to_a_new_subscriber() {
    let hub = Hub::default();
    let session = SessionId::new("sess_1");
    let mut first = hub.subscribe(&session);
    let seq = hub.notify(
        &session,
        AgentId::new("agent_1"),
        "completed",
        "done".to_owned(),
    );
    assert_eq!(seq, 1);
    let Some(AgentSignal::Notice(notice)) = first.next().await else {
        panic!("expected a notice");
    };
    assert_eq!((notice.seq, notice.text.as_str()), (1, "done"));
    assert_eq!(hub.pending(&session), [AgentId::new("agent_1")]);
    // 还没 ack：重新订阅时再次送达。
    let mut second = hub.subscribe(&session);
    assert!(matches!(second.next().await, Some(AgentSignal::Notice(again)) if again.seq == 1));
    hub.ack(&session, 1);
    assert!(hub.pending(&session).is_empty());
    let mut third = hub.subscribe(&session);
    hub.tasks_changed(&session, json!([]));
    assert!(matches!(
        third.next().await,
        Some(AgentSignal::TasksChanged { .. })
    ));
}

/// F11（R1-18）：消费方慢时任务清单只留最新一份，通知一条不丢、按到达顺序交出。旧代码是无界通道，先交出最旧的
/// 任务清单（并把一百个版本都堆在内存里）。
#[tokio::test]
async fn a_slow_consumer_gets_the_latest_task_list_and_every_notice() {
    let hub = Hub::default();
    let session = SessionId::new("sess_1");
    let mut stream = hub.subscribe(&session);
    hub.notify(&session, AgentId::new("a1"), "completed", "one".to_owned());
    for version in 0..100 {
        hub.tasks_changed(&session, json!(version));
    }
    hub.notify(&session, AgentId::new("a2"), "failed", "two".to_owned());
    let mut seen = Vec::new();
    for _ in 0..3 {
        seen.push(stream.next().await.unwrap());
    }
    assert!(matches!(&seen[0], AgentSignal::Notice(notice) if notice.text == "one"));
    assert!(matches!(&seen[1], AgentSignal::TasksChanged { tasks } if tasks == &json!(99)));
    assert!(
        matches!(&seen[2], AgentSignal::Notice(notice) if notice.text == "two" && notice.seq == 2)
    );
    // 没有更多：替换掉的版本不再交出。
    let more = tokio::time::timeout(std::time::Duration::from_millis(50), stream.next()).await;
    assert!(more.is_err());
}

/// 订阅之前的任务清单不重放（与原来只送给当时的订阅者相同）；之后的变化照常送达。
#[tokio::test]
async fn task_lists_from_before_the_subscription_are_not_replayed() {
    let hub = Hub::default();
    let session = SessionId::new("sess_1");
    hub.tasks_changed(&session, json!(["old"]));
    let mut stream = hub.subscribe(&session);
    let nothing = tokio::time::timeout(std::time::Duration::from_millis(50), stream.next()).await;
    assert!(nothing.is_err());
    hub.tasks_changed(&session, json!(["new"]));
    assert!(matches!(
        stream.next().await,
        Some(AgentSignal::TasksChanged { tasks }) if tasks == json!(["new"])
    ));
}
