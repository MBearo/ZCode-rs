use std::sync::Arc;

use rust_cli_kernel_api::message::{CompactTrigger, Message, UserOrigin};
use rust_cli_kernel_api::model::QuerySource;
use rust_cli_kernel_api::testing::{FakeTool, ListTools};
use tokio_util::sync::CancellationToken;

use super::{Engine, Failure};
use crate::test_support::{
    FakeFileStates, Reply, ScriptedRunner, human, no_tools, ports, ports_with_files, reply,
    request, texts, tool_round,
};

fn summary(text: &str) -> Reply {
    Reply::Text(format!("<analysis>a</analysis>\n<summary>{text}</summary>"))
}

#[tokio::test]
async fn short_conversation_is_summarized_whole_without_a_tail() {
    let runner = ScriptedRunner::new(vec![summary("S")]);
    let ports = ports(runner.clone(), no_tools());
    let history = vec![human("u1", "hello"), reply("a1", "hi", None)];
    let result = Engine { ports: &ports }
        .run(
            &request(history, CompactTrigger::Manual),
            true,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result.boundary.trigger, CompactTrigger::Manual);
    assert_eq!(result.boundary.preserved, None);
    assert_eq!(result.boundary.summary, result.summary.id);
    assert_eq!(result.summary.origin, UserOrigin::CompactSummary);
    let text = texts(&Message::User(result.summary));
    assert!(
        text.contains("Summary:\nS\nContinue the conversation"),
        "{text}"
    );
    assert!(!text.contains("kept verbatim"));
    // 摘要请求：同一份消息加摘要提示，来源 compact，最多 1 轮。
    let sent = runner.requests();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].source, QuerySource::Compact);
    assert_eq!(sent[0].max_turns, Some(1));
    let ids: Vec<&str> = sent[0].messages.iter().map(|m| m.id().as_str()).collect();
    assert_eq!(&ids[..2], ["u1", "a1"]);
    assert!(
        texts(sent[0].messages.last().unwrap()).starts_with("CRITICAL: Respond with TEXT ONLY.")
    );
}

#[tokio::test]
async fn longer_conversation_keeps_the_last_round_verbatim() {
    let runner = ScriptedRunner::new(vec![summary("S")]);
    let ports = ports(runner.clone(), no_tools());
    let history = vec![
        human("u1", "first"),
        reply("a1", "one", None),
        human("u2", "second"),
        reply("a2", "two", None),
    ];
    let result = Engine { ports: &ports }
        .run(
            &request(history, CompactTrigger::Auto),
            true,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let preserved = result.boundary.preserved.clone().unwrap();
    assert_eq!(preserved.head.as_str(), "a2");
    assert_eq!(preserved.tail.as_str(), "a2");
    assert_eq!(preserved.anchor, result.summary.id);
    assert!(texts(&Message::User(result.summary)).ends_with("may already have happened in them."));
    let sent = runner.requests();
    let ids: Vec<&str> = sent[0].messages.iter().map(|m| m.id().as_str()).collect();
    assert_eq!(&ids[..3], ["u1", "a1", "u2"]);
}

#[tokio::test]
async fn prompt_too_long_preserves_more_rounds_then_succeeds() {
    let runner = ScriptedRunner::new(vec![
        Reply::PromptTooLong("prompt is too long: 210 tokens > 200 maximum".to_owned()),
        summary("S"),
    ]);
    let ports = ports(runner.clone(), no_tools());
    let mut history = vec![human("u1", "first")];
    for index in 0..4 {
        history.push(reply(&format!("a{index}"), &"x".repeat(400), None));
    }
    let result = Engine { ports: &ports }
        .run(
            &request(history, CompactTrigger::Reactive),
            true,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    // 第一次留 1 组；差 10 token，从摘要部分末尾累计一组就够，第二次留 2 组。
    let sent = runner.requests();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].messages.len(), 4 + 1);
    assert_eq!(sent[1].messages.len(), 3 + 1);
    let preserved = result.boundary.preserved.unwrap();
    assert_eq!(
        (preserved.head.as_str(), preserved.tail.as_str()),
        ("a2", "a3")
    );
}

#[tokio::test]
async fn not_enough_messages_and_exhaustion_use_the_user_texts() {
    let runner = ScriptedRunner::new(Vec::new());
    let ports = ports(runner, no_tools());
    let engine = Engine { ports: &ports };
    let cancel = CancellationToken::new();
    let error = engine
        .run(
            &request(vec![human("u1", "hi")], CompactTrigger::Manual),
            true,
            &cancel,
        )
        .await
        .unwrap_err();
    assert_eq!(error, Failure::NotEnough);
    assert_eq!(error.to_string(), "Not enough messages to compact.");
    assert_eq!(
        engine
            .run(&request(Vec::new(), CompactTrigger::Manual), true, &cancel)
            .await
            .unwrap_err()
            .to_string(),
        "No messages to compact"
    );
    let ptl = || Reply::PromptTooLong("prompt is too long".to_owned());
    let runner = ScriptedRunner::new(vec![ptl(), ptl(), ptl(), ptl(), ptl(), ptl()]);
    let ports = crate::test_support::ports(runner, no_tools());
    let history = vec![
        human("u1", "a"),
        reply("a1", "b", None),
        human("u2", "c"),
        reply("a2", "d", None),
    ];
    let error = Engine { ports: &ports }
        .run(&request(history, CompactTrigger::Reactive), true, &cancel)
        .await
        .unwrap_err();
    assert_eq!(error, Failure::Exhausted);
    assert_eq!(
        error.to_string(),
        "Compaction failed · conversation could not be reduced below the context limit"
    );
}

#[tokio::test]
async fn recently_touched_files_are_reread_with_the_read_tool() {
    let runner = ScriptedRunner::new(vec![summary("S")]);
    let read = Arc::new(FakeTool::new("Read", true, "     1\tfn main() {}"));
    // readFileState 记着的文件（最近的在前）：候选从这里选（compact.md §3.4）。
    let files = FakeFileStates::with(&["/Users/dev/project/b.rs", "/Users/dev/project/a.rs"]);
    let ports = ports_with_files(runner, Arc::new(ListTools(vec![read])), files.clone());
    let mut history = vec![human("u1", "edit things")];
    history.extend(tool_round(1, "Read", "/Users/dev/project/a.rs"));
    history.extend(tool_round(2, "Edit", "/Users/dev/project/b.rs"));
    history.extend(tool_round(3, "Bash", "/ignored"));
    history.push(reply("a_end", "done", None));
    let result = Engine { ports: &ports }
        .run(
            &request(history, CompactTrigger::Manual),
            true,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let restored: Vec<String> = result
        .restored
        .iter()
        .map(|m| match m {
            Message::Attachment(attachment) => {
                assert_eq!(attachment.source, "file");
                attachment.data["filename"].as_str().unwrap().to_owned()
            }
            other => panic!("{other:?}"),
        })
        .collect();
    // 最近的在前；重读前先清空了这个会话的 readFileState。
    assert_eq!(
        restored,
        ["/Users/dev/project/b.rs", "/Users/dev/project/a.rs"]
    );
    assert_eq!(files.cleared.lock().unwrap().len(), 1);
    let text = texts(&result.restored[0]);
    assert_eq!(
        text,
        "<system-reminder>\nCalled the Read tool with the following input: {\"file_path\":\"/Users/dev/project/b.rs\"}\nResult of calling the Read tool:\n     1\tfn main() {}\n</system-reminder>"
    );
}
