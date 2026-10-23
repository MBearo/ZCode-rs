use serde::Serialize;
use serde::de::DeserializeOwned;

use super::*;

fn round_trip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(value: &T) {
    let text = serde_json::to_string(value).unwrap();
    let back: T = serde_json::from_str(&text).unwrap();
    assert_eq!(&back, value, "{text}");
}

fn stored(id: &str) -> StoredCronTask {
    StoredCronTask {
        id: id.to_owned(),
        cron: "*/5 * * * *".to_owned(),
        prompt: "check the deploy".to_owned(),
        created_at: 1_760_000_000_000,
        last_fired_at: Some(1_760_000_300_000),
        recurring: true,
        permanent: false,
        created_by: Some(SessionId::new("sess_a")),
        creator: CronCreator::Gone,
        in_project: true,
    }
}

/// 跨端口的 DTO 都能序列化往返（README §6 规则 8）。
#[test]
fn dtos_round_trip() {
    round_trip(&stored("1a2b3c4d"));
    for creator in [
        CronCreator::ThisProcess,
        CronCreator::Running,
        CronCreator::Gone,
    ] {
        round_trip(&creator);
    }
    let task = NewCronTask {
        id: "1a2b3c4d".to_owned(),
        cron: "0 9 * * 1-5".to_owned(),
        prompt: "line one\nline two".to_owned(),
        created_at: 1,
        recurring: false,
        created_by: SessionId::new("sess_a"),
    };
    for change in [
        CronTasksChange::Add { task },
        CronTasksChange::Remove {
            ids: vec!["1a2b3c4d".to_owned()],
        },
        CronTasksChange::MarkFired {
            ids: vec!["1a2b3c4d".to_owned()],
            at: 2,
        },
        CronTasksChange::Claim {
            session: SessionId::new("sess_a"),
        },
    ] {
        round_trip(&change);
    }
}

/// 旧记录缺省字段时照常能读。
#[test]
fn missing_optional_fields_default() {
    let task: StoredCronTask = serde_json::from_str(
        r#"{"id":"a","cron":"* * * * *","prompt":"p","created_at":1,"creator":"gone"}"#,
    )
    .unwrap();
    assert!(!task.recurring && !task.permanent && !task.in_project);
    assert_eq!((task.last_fired_at, task.created_by), (None, None));
}

/// `session_closed` 的默认实现什么都不做（`NoCron`、旧的实现不受影响）。
#[tokio::test]
async fn session_closed_defaults_to_nothing() {
    use crate::cron::{CronSignals, NoCron};
    let session = SessionId::new("sess_a");
    NoCron.session_closed(&session).await;
    assert!(NoCron.take_due(&session).await.is_empty());
}
