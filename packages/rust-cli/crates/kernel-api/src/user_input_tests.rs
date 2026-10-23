use super::*;

#[test]
fn bash_messages_use_the_tagged_shape() {
    assert_eq!(bash_input("ls -a"), "<bash-input>ls -a</bash-input>");
    let done = BashRun::Completed {
        stdout: "a.txt".to_owned(),
        stderr: String::new(),
    };
    assert_eq!(
        bash_output(&done).as_deref(),
        Some("<bash-stdout>a.txt</bash-stdout><bash-stderr></bash-stderr>")
    );
    let failed = BashRun::Failed {
        message: "no <shell>".to_owned(),
    };
    assert_eq!(
        bash_output(&failed).as_deref(),
        Some("<bash-stderr>Command failed: no &lt;shell&gt;</bash-stderr>")
    );
    assert_eq!(bash_output(&BashRun::Interrupted), None);
}

#[test]
fn bash_messages_read_back_for_display() {
    assert_eq!(
        display("<bash-input>git status</bash-input>").as_deref(),
        Some("! git status")
    );
    assert_eq!(display("hello"), None);
    assert_eq!(
        output_parts("<bash-stdout>out</bash-stdout><bash-stderr>err</bash-stderr>"),
        Some(("out".to_owned(), "err".to_owned()))
    );
    assert_eq!(
        output_parts("<bash-stderr>Command failed: x</bash-stderr>"),
        Some((String::new(), "Command failed: x".to_owned()))
    );
    assert_eq!(output_parts("plain"), None);
}

#[test]
fn escape_xml_only_touches_three_characters() {
    assert_eq!(
        escape_xml("a & b <c> \"d\" 'e'"),
        "a &amp; b &lt;c&gt; \"d\" 'e'"
    );
}

fn human(text: &str) -> UserMessage {
    UserMessage {
        id: crate::ids::MessageId::new("msg_1"),
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        origin: UserOrigin::Human,
    }
}

#[test]
fn only_real_user_input_opens_a_turn() {
    assert!(opens_turn(&human("fix the bug")));
    assert!(opens_turn(&human(&bash_input("ls"))));
    assert!(!opens_turn(&human(INTERRUPT_TEXTS[0])));
    assert!(!opens_turn(&human(INTERRUPT_TEXTS[1])));
    assert!(!opens_turn(&human(
        "<bash-stdout>a</bash-stdout><bash-stderr></bash-stderr>"
    )));
    let notice = UserMessage {
        origin: UserOrigin::Meta {
            source: "task-notification".to_owned(),
        },
        ..human("done")
    };
    assert!(!opens_turn(&notice));
    // 中断提示与别的块一起时是普通输入。
    let mut mixed = human(INTERRUPT_TEXTS[0]);
    mixed.content.push(ContentBlock::Text {
        text: "more".to_owned(),
    });
    assert!(opens_turn(&mixed));
}
