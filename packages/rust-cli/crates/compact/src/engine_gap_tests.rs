//! 初始 token 差（compact.md §3.1）：播种保留的组数，全部摘要时按它裁头。

use rust_cli_kernel_api::message::{CompactTrigger, Message, UserOrigin};
use tokio_util::sync::CancellationToken;

use super::Engine;
use crate::test_support::{
    Reply, ScriptedRunner, human, no_tools, ports, reply, request, texts, tool_round,
};

fn summary() -> Reply {
    Reply::Text("<summary>S</summary>".to_owned())
}

/// 开头一组是很长的人类输入（约 600 token），之后一个 Read 工具轮。
fn c2_history() -> Vec<Message> {
    let mut history = vec![human("u1", &"x".repeat(2_400))];
    history.extend(tool_round(1, "Read", "/Users/dev/project/a.txt"));
    history
}

fn reactive(gap: Option<u64>) -> rust_cli_kernel_api::orchestration::CompactRequest {
    let mut request = request(c2_history(), CompactTrigger::Reactive);
    request.initial_token_gap = gap;
    request
}

/// 两组、差 500：全部摘要时裁掉开头一组，补截断说明；摘要消息带 “Note: the earliest part…”。
#[tokio::test]
async fn the_initial_gap_truncates_the_head_of_a_summarize_all_request() {
    let runner = ScriptedRunner::new(vec![summary()]);
    let ports = ports(runner.clone(), no_tools());
    let result = Engine { ports: &ports }
        .run(&reactive(Some(500)), true, &CancellationToken::new())
        .await
        .unwrap();
    let sent = runner.requests();
    assert_eq!(sent.len(), 1);
    let messages = &sent[0].messages;
    let ids: Vec<&str> = messages.iter().map(|m| m.id().as_str()).collect();
    assert_eq!(ids[1..3], ["a_tool_1", "r_tool_1"], "{ids:?}");
    assert_eq!(messages.len(), 4);
    assert_eq!(
        texts(&messages[0]),
        "[earlier conversation truncated for compaction retry]"
    );
    assert!(matches!(
        &messages[0],
        Message::User(user) if matches!(user.origin, UserOrigin::Meta { .. })
    ));
    let summary = texts(&Message::User(result.summary));
    assert!(
        summary.contains("\n\nNote: the earliest part of the conversation was too large"),
        "{summary}"
    );
    assert_eq!(result.boundary.preserved, None);
}

/// 没有初始差（手动、自动）：全部摘要不裁头，摘要消息不带 Note。
#[tokio::test]
async fn without_a_gap_everything_is_summarized() {
    let runner = ScriptedRunner::new(vec![summary()]);
    let ports = ports(runner.clone(), no_tools());
    let result = Engine { ports: &ports }
        .run(&reactive(None), true, &CancellationToken::new())
        .await
        .unwrap();
    let sent = runner.requests();
    assert_eq!(sent[0].messages.len(), 3 + 1);
    assert_eq!(sent[0].messages[0].id().as_str(), "u1");
    let summary = texts(&Message::User(result.summary));
    assert!(!summary.contains("Note: the earliest part"), "{summary}");
}

/// 差不大于保留部分时不裁（候选减去保留部分的估算 ≤ 0）。
#[tokio::test]
async fn a_gap_covered_by_the_kept_part_does_not_truncate() {
    let runner = ScriptedRunner::new(vec![summary()]);
    let ports = ports(runner.clone(), no_tools());
    let mut history = c2_history();
    // 工具结果之后的人类输入及以后保留：它约 250 token，比差 100 大。
    history.push(human("u2", &"y".repeat(1_000)));
    let mut request = request(history, CompactTrigger::Reactive);
    request.initial_token_gap = Some(100);
    Engine { ports: &ports }
        .run(&request, true, &CancellationToken::new())
        .await
        .unwrap();
    let sent = runner.requests();
    assert_eq!(sent[0].messages[0].id().as_str(), "u1");
}

/// 组数 > 3：差减去最后一组仍 > 0 时按多保留的组数播种，第一次就多保留一组。
#[tokio::test]
async fn more_than_three_groups_seed_the_kept_groups() {
    let history = || {
        vec![
            human("u1", "a"),
            reply("a1", "b", None),
            human("u2", &"c".repeat(400)),
            reply("a2", "d", None),
            human("u3", &"e".repeat(400)),
            reply("a3", "f", None),
            human("u4", &"g".repeat(400)),
            reply("a4", "h", None),
        ]
    };
    let first_request = |gap: Option<u64>| async move {
        let runner = ScriptedRunner::new(vec![summary()]);
        let ports = ports(runner.clone(), no_tools());
        let mut request = request(history(), CompactTrigger::Reactive);
        request.initial_token_gap = gap;
        let result = Engine { ports: &ports }
            .run(&request, true, &CancellationToken::new())
            .await
            .unwrap();
        let preserved = result.boundary.preserved.expect("kept tail");
        (
            runner.requests()[0].messages.len(),
            preserved.head.as_str().to_owned(),
        )
    };
    // 不播种：保留最后一组 [a4]，摘要 7 条。
    assert_eq!(first_request(None).await, (7 + 1, "a4".to_owned()));
    // 差 50，最后一组约 0 token：从 [a3, u4]（约 100 token）累计一组就够，保留 [a3, u4] [a4]。
    assert_eq!(first_request(Some(50)).await, (5 + 1, "a3".to_owned()));
}
