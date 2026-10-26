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

#[test]
fn a_local_command_record_lists_its_messages_in_order() {
    // 修复线 L-A（tui-manage.md §11.1）：固定说明、命令输入、输出，之后是 meta；输出里的 fork skill 启动标签被转义。
    let record = LocalCommandRecord {
        name: "effort".into(),
        args: "high".into(),
        output: LocalCommandOutput::Stdout(
            "Set <forked-skill-launch>x</forked-skill-launch>".into(),
        ),
        caveat: true,
        meta: vec!["note".into()],
    };
    let texts = record.texts();
    assert_eq!(
        texts.iter().map(|(source, _)| *source).collect::<Vec<_>>(),
        [
            LOCAL_COMMAND_CAVEAT_SOURCE,
            LOCAL_COMMAND_INPUT_SOURCE,
            LOCAL_COMMAND_OUTPUT_SOURCE,
            LOCAL_COMMAND_META_SOURCE
        ]
    );
    assert_eq!(texts[0].1, crate::user_input::BASH_CAVEAT);
    assert_eq!(
        texts[1].1,
        "<command-name>/effort</command-name>\n            <command-message>effort</command-message>\n            <command-args>high</command-args>"
    );
    assert_eq!(
        texts[2].1,
        "<local-command-stdout>Set &lt;forked-skill-launch>x&lt;/forked-skill-launch></local-command-stdout>"
    );
    assert_eq!(texts[3].1, "note");
    let failed = LocalCommandRecord {
        output: LocalCommandOutput::Stderr("boom".into()),
        caveat: false,
        meta: Vec::new(),
        ..record
    };
    let texts = failed.texts();
    assert_eq!(texts.len(), 2);
    assert_eq!(
        texts[1].1,
        "<local-command-stderr>boom</local-command-stderr>"
    );
    let json = serde_json::to_value(&failed).unwrap();
    assert!(json.get("meta").is_none(), "{json}");
    assert_eq!(json["output"]["kind"], "stderr");
    assert_eq!(
        serde_json::from_value::<LocalCommandRecord>(json).unwrap(),
        failed
    );
    assert_eq!(bold("x"), "\u{1b}[1mx\u{1b}[22m");
    assert_eq!(dim("x"), "\u{1b}[2mx\u{1b}[22m");
}
