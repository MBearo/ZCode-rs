use super::*;
use crate::message::Usage;
use crate::model::ApiContent;

/// `ALL` 列全了：加一种来源时这个 match 编译失败，`ALL` 与 `from_wire` 一起改。
#[test]
fn every_source_is_listed_and_reads_back() {
    for source in SideSource::ALL {
        match source {
            SideSource::SessionTitle
            | SideSource::Rename
            | SideSource::HookPrompt
            | SideSource::WebFetch
            | SideSource::WebSearch => {}
        }
        assert_eq!(SideSource::from_wire(source.as_wire()), Some(source));
    }
    assert_eq!(SideSource::from_wire("away_summary"), None);
}

/// 来源名与用途名与以前的字符串逐字相同（请求头、用量表与序列化都不变）。
#[test]
fn names_are_the_previous_strings() {
    let table = [
        (
            SideSource::SessionTitle,
            "generate_session_title",
            "session_title",
        ),
        (
            SideSource::Rename,
            "rename_generate_name",
            "rename_generate_name",
        ),
        (SideSource::HookPrompt, "hook_prompt", "hook_prompt"),
        (
            SideSource::WebFetch,
            "web_fetch_apply",
            "web_fetch_processing",
        ),
        (SideSource::WebSearch, "web_search_tool", "web_search_tool"),
    ];
    for (source, wire, node) in table {
        assert_eq!(source.as_wire(), wire);
        assert_eq!(source.node_name(), node);
        assert_eq!(QuerySource::Side(source).node_name(), node);
    }
}

/// `QuerySource` 的序列化与以前的 `Side(String)` 相同；不认识的名字读不出。
#[test]
fn the_query_source_serializes_as_before() {
    let source = QuerySource::Side(SideSource::HookPrompt);
    let text = serde_json::to_string(&source).unwrap();
    assert_eq!(text, r#"{"kind":"side","name":"hook_prompt"}"#);
    assert_eq!(serde_json::from_str::<QuerySource>(&text).unwrap(), source);
    assert!(serde_json::from_str::<QuerySource>(r#"{"kind":"side","name":"title"}"#).is_err());
}

/// 三条规则与以前的名单在现有来源上的结果相同：过载重试的名单里有 hook_prompt、web_search_tool、web_fetch_apply；
/// 安静的名单里只有 hook_prompt；diagnostics 只给 hook_agent（不是现有的来源）。
#[test]
fn the_rules_match_the_previous_lists() {
    let table = [
        (SideSource::SessionTitle, false, false),
        (SideSource::Rename, false, false),
        (SideSource::HookPrompt, true, true),
        (SideSource::WebFetch, true, false),
        (SideSource::WebSearch, true, false),
    ];
    for (source, foreground, quiet) in table {
        assert_eq!(source.foreground(), foreground, "{source:?}");
        assert_eq!(source.quiet(), quiet, "{source:?}");
        assert!(!source.agentic(), "{source:?}");
    }
}

/// 旁路请求的构造：工具、thinking、temperature、beta 都是空的，发请求的一方进 `options`。
#[test]
fn a_side_request_has_no_tools_and_carries_the_requester() {
    let selection = crate::testing::selection();
    let system = vec![SystemBlock {
        text: "system".to_owned(),
        cache_control: None,
    }];
    let request = ModelRequest::side(
        selection.clone(),
        SideSource::WebFetch,
        system.clone(),
        vec![ApiMessage::user_text("hello".to_owned())],
        512,
        Requester {
            session: SessionId::new("sess_1"),
            run: Some(RunId::new("run_1")),
            agent: None,
        },
    );
    let expected = ModelRequest {
        selection,
        system,
        messages: vec![ApiMessage {
            role: ApiRole::User,
            content: vec![ApiContent {
                block: ContentBlock::Text {
                    text: "hello".to_owned(),
                },
                cache_control: None,
            }],
            output_config: None,
        }],
        tools: Vec::new(),
        tool_choice: None,
        max_tokens: 512,
        thinking: None,
        temperature: None,
        betas: Vec::new(),
        source: QuerySource::Side(SideSource::WebFetch),
        options: RequestOptions {
            session_id: Some(SessionId::new("sess_1")),
            run: Some(RunId::new("run_1")),
            ..RequestOptions::default()
        },
    };
    assert_eq!(request, expected);
}

fn usage(input: u64, output: u64) -> Usage {
    Usage {
        input_tokens: input,
        output_tokens: output,
        ..Usage::default()
    }
}

fn events() -> Vec<ModelEvent> {
    vec![
        ModelEvent::MessageStart {
            id: "msg_1".to_owned(),
            model: crate::testing::selection().model,
            usage: usage(10, 0),
        },
        ModelEvent::BlockStart {
            index: 0,
            block: ContentBlock::Thinking {
                thinking: String::new(),
                signature: None,
            },
        },
        ModelEvent::BlockDelta {
            index: 0,
            delta: BlockDelta::Thinking("hmm".to_owned()),
        },
        ModelEvent::BlockStop { index: 0 },
        ModelEvent::BlockStart {
            index: 1,
            block: ContentBlock::Text {
                text: "Hel".to_owned(),
            },
        },
        ModelEvent::BlockDelta {
            index: 1,
            delta: BlockDelta::Text("lo".to_owned()),
        },
        ModelEvent::BlockStop { index: 1 },
        ModelEvent::BlockStart {
            index: 2,
            block: ContentBlock::Text {
                text: String::new(),
            },
        },
        ModelEvent::BlockDelta {
            index: 2,
            delta: BlockDelta::Text(" world".to_owned()),
        },
        ModelEvent::MessageDelta {
            stop_reason: Some(StopReason::EndTurn),
            usage: usage(10, 7),
        },
        ModelEvent::MessageDelta {
            stop_reason: None,
            usage: usage(10, 8),
        },
        ModelEvent::MessageStop,
    ]
}

/// 收集：文字接在同一序号的文字块上，全部文字、第一个块、用量与停止原因。
#[tokio::test]
async fn the_reply_collects_blocks_text_usage_and_the_stop_reason() {
    let stream = futures_util::stream::iter(events().into_iter().map(Ok)).boxed();
    let reply = SideReply::collect(stream).await.unwrap();
    assert_eq!(reply.text(), "Hello world");
    assert_eq!(reply.first_text(), Some(""));
    assert_eq!(reply.usage, usage(10, 8));
    assert_eq!(reply.stop_reason, Some(StopReason::EndTurn));
    assert_eq!(reply.tool_calls(), 0);
    let blocks = reply.into_blocks();
    assert_eq!(blocks.len(), 3);
    assert_eq!(
        blocks[1],
        ContentBlock::Text {
            text: "Hello".to_owned()
        }
    );
    assert!(SideReply::default().first_text().is_none());
}

/// 遇到错误就返回它。
#[tokio::test]
async fn the_first_error_ends_the_collection() {
    let error = ModelError::Network {
        message: "reset".to_owned(),
    };
    let items = vec![Ok(events().remove(0)), Err(error.clone())];
    let stream = futures_util::stream::iter(items).boxed();
    assert_eq!(SideReply::collect(stream).await.unwrap_err(), error);
}
