//! 冷加载的压缩过的会话（docs/specs/rust-cli/compact.md §5.1，工作线 F102）：视图是只供显示的压缩点之前的消息接上
//! 模型侧的历史；请求、rewind 的可选输入只看模型侧的历史。
use std::sync::Arc;

use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::message::{
    AssistantMessage, CompactBoundary, CompactTrigger, ContentBlock, Message, StopReason,
    SystemMessage, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::session_api::SessionHandle;
use rust_cli_kernel_api::testing::{ScriptedModel, text_response};

use crate::Opened;
use crate::test_support::{Setup, finished, submit, until};

fn user(id: &str, text: &str, origin: UserOrigin) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text { text: text.into() }],
        origin,
    })
}

fn reply(id: &str, text: &str) -> Message {
    Message::Assistant(AssistantMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text { text: text.into() }],
        model: "m".into(),
        stop_reason: Some(StopReason::EndTurn),
        usage: None,
    })
}

#[tokio::test]
async fn the_view_shows_the_earlier_part_and_the_model_does_not_see_it() {
    let earlier = vec![
        user("msg_u0", "OLD-QUESTION", UserOrigin::Human),
        reply("msg_a0", "OLD-ANSWER"),
    ];
    let messages = vec![
        Message::System(SystemMessage::CompactBoundary(CompactBoundary {
            id: MessageId::new("msg_b"),
            trigger: CompactTrigger::Manual,
            pre_tokens: 10,
            summary: MessageId::new("msg_s"),
            preserved: None,
            discovered_tools: Vec::new(),
        })),
        user("msg_s", "SUMMARY", UserOrigin::CompactSummary),
        user("msg_u1", "NEW-QUESTION", UserOrigin::Human),
        reply("msg_a1", "NEW-ANSWER"),
    ];
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["next"])]));
    let mut setup = Setup::new(model.clone());
    setup.earlier = earlier.clone();
    setup.messages = messages.clone();
    setup.opened = Opened::Loaded;
    let built = setup.build().await;
    let view = built.session.view().await.unwrap();
    assert_eq!(view.messages, [earlier, messages].concat());
    let options: Vec<MessageId> = built
        .session
        .rewind_options()
        .await
        .unwrap()
        .into_iter()
        .map(|option| option.message)
        .collect();
    assert_eq!(options, [MessageId::new("msg_u1")]);
    let mut events = built.session.subscribe(None);
    built.session.send(submit("c1", "AFTER-RESUME")).await;
    until(&mut events, finished).await;
    let sent = serde_json::to_string(&model.requests()[0].messages).unwrap();
    assert!(
        sent.contains("SUMMARY") && sent.contains("NEW-ANSWER"),
        "{sent}"
    );
    assert!(
        !sent.contains("OLD-QUESTION") && !sent.contains("OLD-ANSWER"),
        "{sent}"
    );
    // 回合之后视图里仍有只供显示的部分，排在最前面。
    let view = built.session.view().await.unwrap();
    assert_eq!(view.messages[0].id().as_str(), "msg_u0");
}
