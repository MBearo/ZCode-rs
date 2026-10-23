use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::message::AttachmentMessage;

use super::*;
use crate::rules::Kind;

fn file(path: &str, kind: Kind, content: &str) -> RuleFile {
    RuleFile {
        path: path.to_owned(),
        kind,
        content: content.to_owned(),
        globs: None,
        parent: None,
    }
}

fn recorded((text, data): (String, Value)) -> Message {
    Message::Attachment(AttachmentMessage {
        id: MessageId::new("m"),
        source: SOURCE.to_owned(),
        text,
        data,
    })
}

const PREAMBLE: &str = "Codebase and user instructions are shown below. Be sure to adhere to these instructions. IMPORTANT: These instructions OVERRIDE any default behavior and you MUST follow them exactly as written.";

#[test]
fn the_first_announcement_lists_every_file_with_its_label() {
    let files = [
        file("/Users/dev/.zcode/AGENTS.md", Kind::User, "user rules\n"),
        file("/w/AGENTS.md", Kind::Project, "\nproject rules\n\n"),
        file("/w/AGENTS.local.md", Kind::Local, "local"),
        file("/etc/zcode/AGENTS.md", Kind::Managed, "managed"),
    ];
    let (text, data) = announce(&[], &files, Reason::SessionStart).unwrap();
    assert_eq!(
        text,
        format!(
            "{PREAMBLE}\n\n\
             Contents of /Users/dev/.zcode/AGENTS.md (user's private global instructions for all projects):\n\nuser rules\n\n\
             Contents of /w/AGENTS.md (project instructions, checked into the codebase):\n\nproject rules\n\n\
             Contents of /w/AGENTS.local.md (user's private project instructions, not checked in):\n\nlocal\n\n\
             Contents of /etc/zcode/AGENTS.md (organization-managed policy instructions):\n\nmanaged"
        )
    );
    assert_eq!(
        data,
        json!({"files": [
            {"path": "/Users/dev/.zcode/AGENTS.md", "type": "User", "content": "user rules"},
            {"path": "/w/AGENTS.md", "type": "Project", "content": "project rules"},
            {"path": "/w/AGENTS.local.md", "type": "Local", "content": "local"},
            {"path": "/etc/zcode/AGENTS.md", "type": "Managed", "content": "managed"},
        ]})
    );
    // 没有规则文件时不发。
    assert!(announce(&[], &[], Reason::SessionStart).is_none());
}

#[test]
fn later_announcements_carry_only_the_differences() {
    let first = [
        file("/w/AGENTS.md", Kind::Project, "v1"),
        file("/w/AGENTS.local.md", Kind::Local, "local"),
    ];
    let history = vec![recorded(
        announce(&[], &first, Reason::SessionStart).unwrap(),
    )];
    assert!(announce(&history, &first, Reason::SessionStart).is_none());

    let second = [
        file("/w/AGENTS.md", Kind::Project, "v2"),
        file("/w/.zcode/AGENTS.md", Kind::Project, "new"),
    ];
    let (text, data) = announce(&history, &second, Reason::SessionStart).unwrap();
    assert_eq!(
        text,
        "Instruction files were re-read when this session started; these differ from their earlier copies \u{2014} each replaces its earlier copy, and the instructions preamble above still applies:\n\n\
         Contents of /w/AGENTS.md (project instructions, checked into the codebase):\n\nv2\n\n\
         Contents of /w/.zcode/AGENTS.md (project instructions, checked into the codebase):\n\nnew\n\n\
         Instructions no longer present: /w/AGENTS.local.md"
    );
    assert_eq!(
        data,
        json!({
            "files": [
                {"path": "/w/AGENTS.md", "type": "Project", "content": "v2"},
                {"path": "/w/.zcode/AGENTS.md", "type": "Project", "content": "new"},
            ],
            "removed": ["/w/AGENTS.local.md"],
            "changed": true,
            "reason": "session_start",
        })
    );

    // 增量叠加在基准上：再来一次相同的内容不再发。
    let mut history = history;
    history.push(recorded((text, data)));
    assert!(announce(&history, &second, Reason::Compaction).is_none());
    let known = announced(&history).unwrap();
    assert_eq!(
        known.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
        ["/w/AGENTS.md", "/w/.zcode/AGENTS.md"]
    );
}

#[test]
fn removals_alone_say_the_files_were_re_read() {
    let first = [file("/w/AGENTS.md", Kind::Project, "v1")];
    let history = vec![recorded(
        announce(&[], &first, Reason::SessionStart).unwrap(),
    )];
    let (text, data) = announce(&history, &[], Reason::Compaction).unwrap();
    assert_eq!(
        text,
        "Instruction files were re-read after the conversation was compacted.\n\nInstructions no longer present: /w/AGENTS.md"
    );
    assert_eq!(data["reason"], "compaction");
    assert_eq!(data["files"], json!([]));
}

#[test]
fn the_memo_is_keyed_by_session_and_compaction_boundary() {
    let memo = Memo::default();
    let session = SessionId::new("s");
    assert!(matches!(
        memo.lookup(&session, None),
        Lookup::Miss(Reason::SessionStart)
    ));
    let files = Arc::new(vec![file("/w/AGENTS.md", Kind::Project, "x")]);
    memo.store(session.clone(), None, files, Reason::SessionStart);
    assert!(matches!(
        memo.lookup(&session, None),
        Lookup::Hit(_, Reason::SessionStart)
    ));
    let boundary = MessageId::new("boundary");
    assert!(matches!(
        memo.lookup(&session, Some(&boundary)),
        Lookup::Miss(Reason::Compaction)
    ));
    assert!(matches!(
        memo.lookup(&SessionId::new("other"), None),
        Lookup::Miss(Reason::SessionStart)
    ));
}
