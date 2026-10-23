use super::*;

#[test]
fn tags_are_written_and_display_restores_the_input() {
    let with_args = command_tags("greet", "Ada Lovelace");
    assert_eq!(
        with_args,
        "<command-message>greet</command-message>\n<command-name>/greet</command-name>\n<command-args>Ada Lovelace</command-args>"
    );
    assert_eq!(display(&with_args).as_deref(), Some("/greet Ada Lovelace"));
    // 没有参数时没有 `<command-args>`（空值不写）。
    let bare = command_tags("helper", "");
    assert_eq!(
        bare,
        "<command-message>helper</command-message>\n<command-name>/helper</command-name>"
    );
    assert_eq!(display(&bare).as_deref(), Some("/helper"));
    assert_eq!(
        display(&command_tags("plugin:skill", "x")).as_deref(),
        Some("/plugin:skill x")
    );
    // 展开失败时的标签顺序同样能还原。
    let failed = command_input("greet", "Ada");
    assert_eq!(
        failed,
        "<command-name>/greet</command-name>\n            <command-message>greet</command-message>\n            <command-args>Ada</command-args>"
    );
    assert_eq!(display(&failed).as_deref(), Some("/greet Ada"));
    assert_eq!(display("/greet Ada"), None);
    assert_eq!(display("hello <command-name>/x</command-name>"), None);
}

#[test]
fn expansions_round_trip_through_json() {
    let expansion = Expansion::Messages {
        messages: vec![
            ExpandedMessage {
                text: command_tags("greet", ""),
                kind: ExpandedKind::Input,
            },
            ExpandedMessage {
                text: "body".into(),
                kind: ExpandedKind::Meta {
                    source: "skill".into(),
                },
            },
        ],
        query: true,
    };
    let json = serde_json::to_value(&expansion).unwrap();
    assert_eq!(json["result"], "messages");
    assert_eq!(json["messages"][1]["kind"], "meta");
    assert_eq!(json["messages"][1]["source"], "skill");
    let back: Expansion = serde_json::from_value(json).unwrap();
    assert_eq!(back, expansion);
    let attachment = ExpandedMessage {
        text: "<system-reminder>\nx\n</system-reminder>".into(),
        kind: ExpandedKind::Attachment {
            source: "unknown_command_fallback".into(),
        },
    };
    let json = serde_json::to_value(&attachment).unwrap();
    assert_eq!(json["kind"], "attachment");
    assert_eq!(
        serde_json::from_value::<ExpandedMessage>(json).unwrap(),
        attachment
    );
}
