use super::*;

fn closed() -> Notice {
    Notice::Closed {
        reason: Some("gone".to_owned()),
        session_expired: false,
    }
}

/// F11（R1-18）：连接任务忙时，同一种 `list_changed` 只留一条（收到后重新拉整份清单），关闭排在最后且不丢。
#[tokio::test]
async fn list_changes_merge_by_kind_and_close_comes_last() {
    let (sender, mut receiver) = channel();
    for _ in 0..1000 {
        sender.send(Notice::ListChanged("tools".to_owned()));
        sender.send(Notice::ListChanged("prompts".to_owned()));
    }
    sender.send(closed());
    drop(sender);
    let mut seen = Vec::new();
    while let Some(notice) = receiver.recv().await {
        seen.push(notice);
    }
    assert_eq!(
        seen,
        [
            Notice::ListChanged("tools".to_owned()),
            Notice::ListChanged("prompts".to_owned()),
            closed(),
        ]
    );
}

/// 取走之后再来的同一种照常送达。
#[tokio::test]
async fn a_kind_taken_away_can_arrive_again() {
    let (sender, mut receiver) = channel();
    sender.send(Notice::ListChanged("tools".to_owned()));
    assert_eq!(
        receiver.recv().await,
        Some(Notice::ListChanged("tools".to_owned()))
    );
    sender.send(Notice::ListChanged("tools".to_owned()));
    assert_eq!(
        receiver.recv().await,
        Some(Notice::ListChanged("tools".to_owned()))
    );
}
