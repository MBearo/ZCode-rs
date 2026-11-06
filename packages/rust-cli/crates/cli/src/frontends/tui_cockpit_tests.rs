//! 两个笔记的读写（tui-cockpit.md §1）。
use super::*;

fn cockpit(dir: &std::path::Path) -> LocalCockpit {
    LocalCockpit {
        dir: dir.join("cockpit"),
        peers: dir.join("peers"),
        tmux_socket: None,
    }
}

#[tokio::test]
async fn notes_round_trip_and_forget_removes_both() {
    let dir = tempfile::tempdir().unwrap();
    let port = cockpit(dir.path());
    let id = SessionId::new("a");
    port.write_turn(id.clone(), 10, Some("Rate limited.".to_owned()))
        .await;
    port.mark_seen(id.clone(), 5).await;
    let notes = port.notes(vec![id.clone(), SessionId::new("none")]).await;
    assert_eq!(notes.len(), 1);
    assert_eq!(
        notes[&id],
        CockpitNote {
            ended_at_ms: Some(10),
            failed: Some("Rate limited.".to_owned()),
            seen_at_ms: Some(5),
        }
    );
    port.forget(id.clone()).await;
    assert!(port.notes(vec![id]).await.is_empty());
}
