use rust_cli_kernel_api::ids::ToolUseId;
use rust_cli_kernel_api::message::{ContentBlock, StopReason, Usage};
use rust_cli_kernel_api::model::{BlockDelta, ModelEvent};
use serde_json::json;

use super::*;

fn start() -> ModelEvent {
    ModelEvent::MessageStart {
        id: "msg_api".to_owned(),
        model: "m".to_owned(),
        usage: Usage {
            input_tokens: 10,
            ..Usage::default()
        },
    }
}

#[test]
fn text_thinking_and_tool_use_blocks_assemble_in_order() {
    let mut assembler = Assembler::default();
    for event in [
        start(),
        ModelEvent::BlockStart {
            index: 0,
            block: ContentBlock::Thinking {
                thinking: String::new(),
                signature: None,
            },
        },
        ModelEvent::BlockDelta {
            index: 0,
            delta: BlockDelta::Thinking("plan".to_owned()),
        },
        ModelEvent::BlockDelta {
            index: 0,
            delta: BlockDelta::Signature("sig".to_owned()),
        },
        ModelEvent::BlockStop { index: 0 },
        ModelEvent::BlockStart {
            index: 1,
            block: ContentBlock::Text {
                text: String::new(),
            },
        },
        ModelEvent::BlockDelta {
            index: 1,
            delta: BlockDelta::Text("he".to_owned()),
        },
        ModelEvent::BlockDelta {
            index: 1,
            delta: BlockDelta::Text("llo".to_owned()),
        },
        ModelEvent::BlockStop { index: 1 },
        ModelEvent::BlockStart {
            index: 2,
            block: ContentBlock::ToolUse {
                id: ToolUseId::new("toolu_1"),
                name: "Read".to_owned(),
                input: json!({}),
            },
        },
        ModelEvent::BlockDelta {
            index: 2,
            delta: BlockDelta::InputJson("{\"file_path\":".to_owned()),
        },
        ModelEvent::BlockDelta {
            index: 2,
            delta: BlockDelta::InputJson("\"a.rs\"}".to_owned()),
        },
        ModelEvent::BlockStop { index: 2 },
        ModelEvent::MessageDelta {
            stop_reason: Some(StopReason::ToolUse),
            usage: Usage {
                output_tokens: 5,
                ..Usage::default()
            },
        },
        ModelEvent::MessageStop,
    ] {
        assembler.push(event).unwrap();
    }
    assert!(assembler.incomplete().is_none());
    let assembled = assembler.finish();
    assert_eq!(assembled.stop_reason, Some(StopReason::ToolUse));
    assert_eq!(assembled.usage.input_tokens, 10);
    assert_eq!(assembled.usage.output_tokens, 5);
    assert_eq!(
        assembled.content,
        vec![
            ContentBlock::Thinking {
                thinking: "plan".to_owned(),
                signature: Some("sig".to_owned()),
            },
            ContentBlock::Text {
                text: "hello".to_owned()
            },
            ContentBlock::ToolUse {
                id: ToolUseId::new("toolu_1"),
                name: "Read".to_owned(),
                input: json!({"file_path": "a.rs"}),
            },
        ]
    );
}

#[test]
fn empty_tool_input_is_an_empty_object() {
    let mut assembler = Assembler::default();
    for event in [
        start(),
        ModelEvent::BlockStart {
            index: 0,
            block: ContentBlock::ToolUse {
                id: ToolUseId::new("toolu_1"),
                name: "EnterPlanMode".to_owned(),
                input: json!({}),
            },
        },
        ModelEvent::BlockStop { index: 0 },
        ModelEvent::MessageStop,
    ] {
        assembler.push(event).unwrap();
    }
    assert!(assembler.incomplete().is_none());
    let assembled = assembler.finish();
    assert!(matches!(
        &assembled.content[0],
        ContentBlock::ToolUse { input, .. } if input == &json!({})
    ));
}

#[test]
fn a_stream_without_message_stop_is_an_error() {
    let mut assembler = Assembler::default();
    assembler.push(start()).unwrap();
    assert!(assembler.incomplete().is_some());
}

/// R5-1：输入不是合法 JSON 时换成标记对象（原文前 2048 个 UTF-16 单元与总长度），块照常结束，
/// 交给执行管线的 schema 校验；不再让整个回合失败。
#[test]
fn unparsable_tool_input_becomes_the_unparsed_marker() {
    let raw = "{\"file_path\": \"/work/x.txt\"";
    let mut assembler = Assembler::default();
    let mut finished = None;
    for event in [
        start(),
        ModelEvent::BlockStart {
            index: 0,
            block: ContentBlock::ToolUse {
                id: ToolUseId::new("toolu_1"),
                name: "Read".to_owned(),
                input: json!({}),
            },
        },
        ModelEvent::BlockDelta {
            index: 0,
            delta: BlockDelta::InputJson(raw.to_owned()),
        },
        ModelEvent::BlockStop { index: 0 },
        ModelEvent::MessageStop,
    ] {
        if let Some(block) = assembler.push(event).unwrap() {
            finished = Some(block);
        }
    }
    let marker = json!({"__unparsedToolInput": {"raw": raw, "len": raw.len()}});
    assert!(matches!(
        &finished,
        Some(ContentBlock::ToolUse { input, .. }) if input == &marker
    ));
    assert!(assembler.incomplete().is_none());
    assert!(matches!(
        &assembler.finish().content[0],
        ContentBlock::ToolUse { input, .. } if input == &marker
    ));
}

/// 原文超过 2048 个 UTF-16 单元时截断（不拆开代理对），`len` 是原文的 UTF-16 长度；
/// 字面量 `null` 当作 `{}`。
#[test]
fn the_marker_keeps_a_utf16_prefix_and_null_is_an_empty_object() {
    let finish = |raw: &str| {
        let block = ContentBlock::ToolUse {
            id: ToolUseId::new("toolu_1"),
            name: "Bash".to_owned(),
            input: json!({}),
        };
        let mut assembler = Assembler::default();
        assembler.push(start()).unwrap();
        assembler
            .push(ModelEvent::BlockStart { index: 0, block })
            .unwrap();
        assembler
            .push(ModelEvent::BlockDelta {
                index: 0,
                delta: BlockDelta::InputJson(raw.to_owned()),
            })
            .unwrap();
        match assembler.push(ModelEvent::BlockStop { index: 0 }).unwrap() {
            Some(ContentBlock::ToolUse { input, .. }) => input,
            other => panic!("{other:?}"),
        }
    };
    // 2047 个 ASCII 之后是一个代理对：前缀停在 2047，不留半个字符。
    let raw = format!("{{{}😀tail", "a".repeat(2046));
    let input = finish(&raw);
    let marker = &input["__unparsedToolInput"];
    assert_eq!(marker["raw"], json!(raw[..2047]));
    assert_eq!(marker["len"], json!(raw.encode_utf16().count()));
    assert_eq!(finish(" null "), json!({}));
}

/// 主线程被中止时（修复线 F38，query.md §1.4）：已结束的块之后带上没写完的文字（`trim` 后不为空）；
/// 没写完的 thinking、tool_use 与只有空白的文字不留。
#[test]
fn an_aborted_stream_keeps_the_unfinished_text_only() {
    let text = |index| ModelEvent::BlockStart {
        index,
        block: ContentBlock::Text {
            text: String::new(),
        },
    };
    let more = |index, value: &str| ModelEvent::BlockDelta {
        index,
        delta: BlockDelta::Text(value.to_owned()),
    };
    let assemble = |events: Vec<ModelEvent>| {
        let mut assembler = Assembler::default();
        for event in events {
            assembler.push(event).unwrap();
        }
        assembler.finish_aborted().content
    };
    let done_then_open = assemble(vec![
        start(),
        text(0),
        more(0, "done"),
        ModelEvent::BlockStop { index: 0 },
        text(1),
        more(1, "Partial answer"),
    ]);
    assert_eq!(
        done_then_open,
        [
            ContentBlock::Text {
                text: "done".to_owned()
            },
            ContentBlock::Text {
                text: "Partial answer".to_owned()
            }
        ]
    );
    let blank = assemble(vec![start(), text(0), more(0, "  \n")]);
    assert!(blank.is_empty(), "{blank:?}");
    let thinking = assemble(vec![
        start(),
        ModelEvent::BlockStart {
            index: 0,
            block: ContentBlock::Thinking {
                thinking: String::new(),
                signature: None,
            },
        },
        ModelEvent::BlockDelta {
            index: 0,
            delta: BlockDelta::Thinking("plan".to_owned()),
        },
    ]);
    assert!(thinking.is_empty(), "{thinking:?}");
    let tool = assemble(vec![
        start(),
        ModelEvent::BlockStart {
            index: 0,
            block: ContentBlock::ToolUse {
                id: ToolUseId::new("t1"),
                name: "Bash".to_owned(),
                input: json!({}),
            },
        },
        ModelEvent::BlockDelta {
            index: 0,
            delta: BlockDelta::InputJson("{\"command\":\"ec".to_owned()),
        },
    ]);
    assert!(tool.is_empty(), "{tool:?}");
}

fn text_block(index: usize, value: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::BlockStart {
            index,
            block: ContentBlock::Text {
                text: String::new(),
            },
        },
        ModelEvent::BlockDelta {
            index,
            delta: BlockDelta::Text(value.to_owned()),
        },
        ModelEvent::BlockStop { index },
    ]
}

fn assembled(events: Vec<ModelEvent>) -> Assembler {
    let mut assembler = Assembler::default();
    for event in events {
        assembler.push(event).unwrap();
    }
    assembler
}

/// 工作线 F90（query.md §1.9）：模型适配器跳过的块不输出任何事件，它的序号在组装器里是空槽。空槽不是没结束的块：
/// 修 bug 前流正常结束也报 `stream ended with an unfinished block`，中断时停在空槽、截掉后面已经流出的文字。
#[test]
fn a_sequence_number_that_never_started_is_not_an_unfinished_block() {
    let mut events = vec![start()];
    // index 1 被跳过；index 3 在 index 2 之前开始（乱序）。
    events.extend(text_block(0, "a"));
    events.extend(text_block(3, "c"));
    events.extend(text_block(2, "b"));
    events.extend([
        ModelEvent::MessageDelta {
            stop_reason: Some(StopReason::EndTurn),
            usage: Usage::default(),
        },
        ModelEvent::MessageStop,
    ]);
    let assembler = assembled(events);
    assert_eq!(assembler.incomplete(), None);
    let texts = |content: Vec<ContentBlock>| -> Vec<String> {
        content
            .into_iter()
            .map(|block| match block {
                ContentBlock::Text { text } => text,
                other => panic!("{other:?}"),
            })
            .collect()
    };
    assert_eq!(texts(assembler.finish().content), ["a", "b", "c"]);

    // 中断：空槽之后已经结束的块与没写完的文字都留下。
    let mut events = vec![start()];
    events.extend(text_block(0, "a"));
    events.extend(text_block(2, "b"));
    events.extend([
        ModelEvent::BlockStart {
            index: 3,
            block: ContentBlock::Text {
                text: String::new(),
            },
        },
        ModelEvent::BlockDelta {
            index: 3,
            delta: BlockDelta::Text("partial".to_owned()),
        },
    ]);
    assert_eq!(
        texts(assembled(events).finish_aborted().content),
        ["a", "b", "partial"]
    );
}

/// 工作线 F90（query.md §1.9、tools-more.md §5.1）：回复里端点自己产生的服务端块（`server_tool_use`、
/// `web_search_tool_result`、assistant 一侧的 `tool_result`）不进回复，照 Node 不落库、不发回；收不到结束的服务端块
/// 不让这次响应失败。修 bug 前它们留在回复里，node-store 拒收，整条回复与工具结果都没落库。
#[test]
fn server_blocks_are_not_part_of_the_reply() {
    let server = |index, block| {
        vec![
            ModelEvent::BlockStart { index, block },
            ModelEvent::BlockStop { index },
        ]
    };
    let mut events = vec![start()];
    events.extend(text_block(0, "before"));
    events.extend(server(
        1,
        ContentBlock::ServerToolUse {
            id: "srvtoolu_1".to_owned(),
            name: "web_search".to_owned(),
            input: json!({"query": "q"}),
        },
    ));
    events.extend(server(
        2,
        ContentBlock::ToolResult {
            tool_use_id: ToolUseId::new("srvtoolu_1"),
            content: Vec::new(),
            is_error: false,
        },
    ));
    events.extend(server(
        3,
        ContentBlock::WebSearchToolResult {
            tool_use_id: "srvtoolu_1".to_owned(),
            content: json!([]),
        },
    ));
    events.extend(text_block(4, "after"));
    // 收不到结束的服务端块也不算没结束的块。
    events.push(ModelEvent::BlockStart {
        index: 5,
        block: ContentBlock::ServerToolUse {
            id: "srvtoolu_2".to_owned(),
            name: "web_search".to_owned(),
            input: json!({}),
        },
    });
    events.push(ModelEvent::MessageStop);
    let assembler = assembled(events);
    assert_eq!(assembler.incomplete(), None);
    assert_eq!(
        assembler.finish().content,
        [
            ContentBlock::Text {
                text: "before".to_owned()
            },
            ContentBlock::Text {
                text: "after".to_owned()
            },
        ]
    );
}
