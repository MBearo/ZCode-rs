use super::*;
use crate::ids::TurnId;
use crate::message::{ContentBlock, UserMessage, UserOrigin};

fn user(id: &str, text: &str, origin: UserOrigin) -> Fact {
    Fact::MessageAppended {
        message: Message::User(UserMessage {
            id: MessageId::new(id),
            content: vec![ContentBlock::Text {
                text: text.to_owned(),
            }],
            origin,
        }),
    }
}

fn anchor(prompt: &str, turn: &str) -> TurnAnchor {
    TurnAnchor {
        prompt: MessageId::new(prompt),
        turn: Some(TurnId::new(turn)),
    }
}

#[test]
fn a_new_prompt_starts_a_new_anchor_and_other_batches_keep_the_current_one() {
    let current = anchor("msg_old", "turn_old");
    let fresh = || TurnId::new("turn_new");
    let prompt = user("msg_new", "hello", UserOrigin::Human);
    assert_eq!(
        TurnAnchor::after(Some(&current), &[prompt], fresh),
        Some(anchor("msg_new", "turn_new"))
    );
    // 通知、中断提示与其他事实不换轮次。
    let notice = user(
        "msg_n",
        "done",
        UserOrigin::Meta {
            source: "task-notification".to_owned(),
        },
    );
    let interrupt = user(
        "msg_i",
        crate::user_input::INTERRUPT_TEXTS[0],
        UserOrigin::Human,
    );
    assert_eq!(
        TurnAnchor::after(Some(&current), &[notice, interrupt], fresh),
        Some(current.clone())
    );
    assert_eq!(TurnAnchor::after(None, &[], fresh), None);
    // 几条输入（复制历史）：一个锚点表达不了，交给存储。
    let two = [
        user("msg_a", "a", UserOrigin::Human),
        user("msg_b", "b", UserOrigin::Human),
    ];
    assert_eq!(TurnAnchor::after(Some(&current), &two, fresh), None);
}

/// 只实现必需方法的 ArtifactStore：`find` 走缺省（F18）。
struct PutOnly;

#[async_trait]
impl ArtifactStore for PutOnly {
    async fn put(
        &self,
        _session: &SessionId,
        _purpose: ArtifactPurpose,
        _media_type: String,
        _bytes: Vec<u8>,
    ) -> Result<ArtifactRef, StoreError> {
        Err(StoreError::Unsupported {
            what: "put".to_owned(),
        })
    }

    async fn get(&self, _artifact: &ArtifactRef) -> Result<Vec<u8>, StoreError> {
        Err(StoreError::Unsupported {
            what: "get".to_owned(),
        })
    }
}

#[tokio::test]
async fn finding_an_artifact_is_unsupported_unless_the_store_implements_it() {
    let found = PutOnly
        .find(&SessionId::new("sess_1"), &ArtifactId::new("tool-result-1"))
        .await;
    assert!(
        matches!(found, Err(StoreError::Unsupported { ref what }) if what == "artifact lookup by id"),
        "{found:?}"
    );
}
