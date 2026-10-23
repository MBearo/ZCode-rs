use super::*;

fn assistant(text: &str) -> Message {
    Message::Assistant(rust_cli_kernel_api::message::AssistantMessage {
        id: rust_cli_kernel_api::ids::MessageId::new("a"),
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        model: "m".to_owned(),
        stop_reason: None,
        usage: None,
    })
}

#[test]
fn summary_prefers_the_reply_with_a_summary_block() {
    let messages = [
        assistant("<summary>S</summary>"),
        assistant("later chatter"),
    ];
    assert_eq!(
        summary_of(&messages).map(|(text, _)| text),
        Some("<summary>S</summary>".to_owned())
    );
    assert_eq!(
        summary_of(&[assistant("plain")]).map(|(text, _)| text),
        Some("plain".to_owned())
    );
    assert_eq!(summary_of(&[assistant("  ")]), None);
}
