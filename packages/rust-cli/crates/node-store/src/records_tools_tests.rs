//! 工具会话的事实 → Node 记录逐字节比对（docs/specs/rust-cli/node-store.md §4.6、§6）：
//! `fixtures/node-records-tools.json` 由 `scripts/rust-cli/node-store-fixtures-tools.mjs` 用 Node 运行时真实的
//! 写入函数（persistPendingToolPart、settleToolResults、persistToolModelStepFinish 等）在固定时钟与 uuid 下生成；
//! 这里用 NodeStore 按 query 主循环真实的提交顺序与批次提交同一组事实，各表的行逐字节相同。
//! 两边的场景与时间线一一对应，改一边时同步改另一边。
use rust_cli_base::js::json::stringify;
use rust_cli_kernel_api::ids::{MessageId, OperationId, SessionId, ToolUseId};
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, ImageSource, Message, StopReason, ToolResultContent, Usage,
    UserMessage, UserOrigin,
};
use rust_cli_kernel_api::metering::{CallStatus, ModelCall, ToolCall};
use rust_cli_kernel_api::model::QuerySource;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::store::{Fact, SessionStore};
use serde_json::{Value, json};

use crate::tests::{TestStore, batch, meta, selection, text, user};

const SESSION: &str = "sess_tools";
const PNG: &str = "iVBORw0KGgo=";

fn fixture() -> Value {
    serde_json::from_str(include_str!("../fixtures/node-records-tools.json")).unwrap()
}

fn tool_use(id: &str, name: &str, input: Value) -> ContentBlock {
    ContentBlock::ToolUse {
        id: ToolUseId::new(id),
        name: name.into(),
        input,
    }
}

/// 一次模型请求的 assistant 与它的计量（query `commit_metered`：同一批）。
fn step(id: &str, content: Vec<ContentBlock>, usage: Usage, stop: StopReason) -> Vec<Fact> {
    let (started, completed) = match id {
        "msg_rv_a1" => (1003, 1010),
        "msg_sn_a2" => (1031, 1040),
        _ => (1041, 1050),
    };
    let tool_calls = content
        .iter()
        .filter(|block| matches!(block, ContentBlock::ToolUse { .. }))
        .count();
    let call = ModelCall {
        message: MessageId::new(id),
        source: QuerySource::MainThread,
        selection: selection(),
        started_at_ms: started,
        first_token_at_ms: Some(started + 1),
        first_content_at_ms: None,
        completed_at_ms: completed,
        status: CallStatus::Completed,
        stop_reason: Some(stop),
        tool_calls: u32::try_from(tool_calls).unwrap(),
        usage: Some(usage),
        retries: 0,
        error: None,
    };
    let assistant = Message::Assistant(AssistantMessage {
        id: MessageId::new(id),
        content,
        model: "m".into(),
        stop_reason: Some(stop),
        usage: Some(usage),
    });
    vec![
        Fact::MessageAppended { message: assistant },
        Fact::ModelCalled { call },
    ]
}

/// 一个工具结果消息与它的计量（query `commit_metered(…, Metered::Tool)`：同一批）。
fn result(
    (id, owner): (&str, &str),
    (call, name): (&str, &str),
    content: Vec<ToolResultContent>,
    (started, completed, is_error): (u64, u64, bool),
) -> Vec<Fact> {
    let output_bytes = content
        .iter()
        .map(|block| match block {
            ToolResultContent::Text { text } => text.len() as u64,
            _ => 0,
        })
        .sum();
    let message = Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::ToolResult {
            tool_use_id: ToolUseId::new(call),
            content,
            is_error,
        }],
        origin: UserOrigin::ToolResult {
            assistant: MessageId::new(owner),
        },
    });
    let status = if is_error {
        CallStatus::Error
    } else {
        CallStatus::Completed
    };
    let call = ToolCall {
        tool_use: ToolUseId::new(call),
        name: name.into(),
        started_at_ms: started,
        completed_at_ms: completed,
        status,
        output_bytes,
    };
    vec![Fact::MessageAppended { message }, Fact::ToolCalled { call }]
}

/// query 执行器在工具开工前提交的意图（fingerprint 是 `{"tool", "input"}` 的 JSON）。
fn intent(operation: &str, name: &str, input: &Value) -> Vec<Fact> {
    vec![Fact::OperationIntent {
        operation: OperationId::new(operation),
        fingerprint: stringify(&json!({"tool": name, "input": input})),
    }]
}

/// 与夹具脚本同一条时间线：两个并发工具（A 先声明、最后结束，B 后声明、先结束）在流式响应中开工，
/// 结果各自到达时提交；第二步的工具开工即失败；最后的回答之后回合结束。返回提交的消息（提交顺序）。
/// 本工程分配 uuid 的顺序（夹具脚本按它排 Node 的 uuid）：1 轮次、2 输入的 text part；a1 的 step-start 3、
/// text 4、tool part 5（A）与 6（B）、step-finish 7；A 结果的图片 8；a2 的 step-start 9、reasoning 10、
/// tool part 11、step-finish 12；a3 的 13 到 15。
async fn replay(t: &TestStore) -> Vec<Message> {
    let session = SessionId::new(SESSION);
    let committed = std::cell::RefCell::new(Vec::new());
    let commit = |now: u64, command: Option<&'static str>, facts: Vec<Fact>| {
        t.source.at(now);
        committed
            .borrow_mut()
            .extend(facts.iter().filter_map(|fact| match fact {
                Fact::MessageAppended { message } => Some(message.clone()),
                _ => None,
            }));
        let session = session.clone();
        async move {
            t.store
                .commit(&session, batch(command, facts))
                .await
                .unwrap()
        }
    };
    t.source.at(1000);
    t.store.create(meta(SESSION, "/w/repo", "")).await.unwrap();
    let selected = Fact::ModelSelected {
        selection: selection(),
    };
    let mode = Fact::ModeChanged {
        mode: PermissionMode::Default,
    };
    commit(1001, None, vec![selected, mode]).await;
    commit(
        1002,
        Some("cmd_1"),
        vec![user("msg_ru_u1", "Show logo.png and a.txt")],
    )
    .await;

    let read_logo = json!({"file_path": "/w/repo/logo.png"});
    let cat = json!({"command": "cat a.txt", "description": "Show a.txt"});
    commit(1004, None, intent("op_a", "Read", &read_logo)).await;
    commit(1005, None, intent("op_b", "Bash", &cat)).await;
    let usage = |input, output, write, read| Usage {
        input_tokens: input,
        output_tokens: output,
        cache_creation_input_tokens: write,
        cache_read_input_tokens: read,
    };
    let first = vec![
        text("I'll look at both."),
        tool_use("toolu_a", "Read", read_logo),
        tool_use("toolu_b", "Bash", cat),
    ];
    let first = step("msg_rv_a1", first, usage(10, 5, 3, 2), StopReason::ToolUse);
    commit(1010, None, first).await;
    // 1020 = "sc"、1030 = "sm"（36 进制）：结果消息 id 里的时间是结果产生的时刻。
    let said = |text: &str| ToolResultContent::Text { text: text.into() };
    let hello = result(
        ("msg_sc_rb", "msg_rv_a1"),
        ("toolu_b", "Bash"),
        vec![said("hello from a.txt")],
        (1005, 1020, false),
    );
    commit(1020, None, hello).await;
    let image = ToolResultContent::Image {
        source: ImageSource::Base64 {
            media_type: "image/png".into(),
            data: PNG.into(),
        },
    };
    let logo = result(
        ("msg_sm_ra", "msg_rv_a1"),
        ("toolu_a", "Read"),
        vec![said("Image: logo.png"), image],
        (1004, 1030, false),
    );
    commit(1030, None, logo).await;

    let missing = json!({"file_path": "/w/repo/missing.txt"});
    commit(1040, None, intent("op_c", "Read", &missing)).await;
    let thinking = ContentBlock::Thinking {
        thinking: "Check the other file".into(),
        signature: Some("sig-2".into()),
    };
    let second = vec![thinking, tool_use("toolu_c", "Read", missing)];
    let second = step(
        "msg_sn_a2",
        second,
        usage(28, 6, 0, 12),
        StopReason::ToolUse,
    );
    commit(1040, None, second).await;
    let failed = result(
        ("msg_sw_rc", "msg_sn_a2"),
        ("toolu_c", "Read"),
        vec![said(
            "<tool_use_error>File does not exist.</tool_use_error>",
        )],
        (1040, 1040, true),
    );
    commit(1040, None, failed).await;

    let answer = vec![text("logo.png is an image; a.txt says hello.")];
    let answer = step("msg_sx_a3", answer, usage(60, 9, 0, 0), StopReason::EndTurn);
    commit(1050, None, answer).await;
    let ended = Fact::TurnEnded {
        status: CallStatus::Completed,
    };
    commit(1051, None, vec![ended]).await;
    committed.into_inner()
}

/// part 行的 data 去掉 `metadata.rustCli`：本工程在 part 的非 strict 区域记结果消息的 id 与提交序号
/// （node-store.md §4.6 第 5 条），Node 没有这个键。只去掉这一个键，其余原样（键序不变）。
fn without_rust_cli(mut row: Vec<Value>) -> Vec<Value> {
    let mut data: Value = serde_json::from_str(row[5].as_str().unwrap()).unwrap();
    if let Some(metadata) = data.get_mut("metadata").and_then(Value::as_object_mut) {
        metadata.remove("rustCli");
    }
    row[5] = stringify(&data).into();
    row
}

/// 一处已登记的差异：表、行 id、列号、data 列里的 JSON 指针（None 是整列）、本工程的值、Node 的值。
type Registered = (
    &'static str,
    &'static str,
    usize,
    Option<&'static str>,
    Value,
    Value,
);

const A1_STEP_FINISH: &str = "part_s2_00000000-0000-4000-8000-000000000007";
const NODE_ARTIFACT: &str =
    "zcode-artifact://sess_tools/tool-result-00000000-0000-4000-8000-000000000901";

/// node-store.md §7 已登记的差异。比较前把本工程的行在这些位置换成 Node 的值：每一处先断言本工程写的
/// 是登记的值，换完后整行逐字节比对，登记以外的任何不同都让测试失败。
fn registered() -> Vec<Registered> {
    vec![
        // §7「assistant 一次写入」：本工程在流结束时写完整的 assistant 与 step-start / step-finish（1010）。
        // Node 的工具步骤在工具都结算后（1030）才写 step-finish（新 part id，id 里带写入时间），并重写
        // assistant 的 `time.completed`（`persistToolModelStepFinish`）。没有工具的步骤两边相同。
        ("message", "msg_rv_a1", 3, None, json!(1010), json!(1030)),
        (
            "message",
            "msg_rv_a1",
            4,
            Some("/time/completed"),
            json!(1010),
            json!(1030),
        ),
        ("part", A1_STEP_FINISH, 3, None, json!(1010), json!(1030)),
        ("part", A1_STEP_FINISH, 4, None, json!(1010), json!(1030)),
        (
            "part",
            A1_STEP_FINISH,
            0,
            None,
            json!(A1_STEP_FINISH),
            json!("part_sm_00000000-0000-4000-8000-000000000007"),
        ),
        // §7「工具结果的落库时机」（F12 新登记）：本工程每个结果到达时一个事务（B 在 1020）；Node 等整批工具
        // 结束（1030）才按声明顺序结算，part 行的 time_updated 是结算时间。`state.time` 两边相同。
        (
            "part",
            "part_s2_00000000-0000-4000-8000-000000000006",
            4,
            None,
            json!(1020),
            json!(1030),
        ),
        // §7「工具结果里的内联图片」：本工程把内联图片存成 data URL；Node 的 `persistToolResultMediaAttachments`
        // 总把工具媒体写成 artifact 文件（内容是 data URL，没有 artifact store 时报错），file part 引用它。
        (
            "part",
            "part_s2_00000000-0000-4000-8000-000000000005",
            5,
            Some("/state/attachments/0/url"),
            json!(format!("data:image/png;base64,{PNG}")),
            json!(NODE_ARTIFACT),
        ),
        (
            "part",
            "part_s2_00000000-0000-4000-8000-000000000005",
            5,
            Some("/state/attachments/0/metadata"),
            json!({"recoverability": "provider_ready", "storageKind": "inline"}),
            json!({"artifactUri": NODE_ARTIFACT, "recoverability": "provider_ready",
                "storageKind": "artifact"}),
        ),
    ]
}

fn apply_registered(name: &str, rows: &mut [Vec<Value>]) {
    for (table, id, column, pointer, ours, node) in registered() {
        if table != name {
            continue;
        }
        let row = rows
            .iter_mut()
            .find(|row| row[0] == id)
            .unwrap_or_else(|| panic!("{table}: no row {id}"));
        match pointer {
            None => {
                assert_eq!(row[column], ours, "{table} {id} column {column}");
                row[column] = node;
            }
            Some(pointer) => {
                let mut data: Value = serde_json::from_str(row[column].as_str().unwrap()).unwrap();
                let value = data.pointer_mut(pointer).unwrap();
                assert_eq!(*value, ours, "{table} {id} {pointer}");
                *value = node;
                row[column] = stringify(&data).into();
            }
        }
    }
}

#[tokio::test]
async fn a_tool_turn_is_stored_like_node_byte_for_byte() {
    let t = TestStore::new();
    replay(&t).await;
    let fixture = fixture();
    let parts = t
        .rows("select * from part order by message_id, sequence")
        .into_iter()
        .map(without_rust_cli)
        .collect();
    let tables = [
        ("session", t.rows("select * from session")),
        ("message", t.rows("select * from message order by sequence")),
        ("part", parts),
        (
            "session_entry",
            t.rows("select * from session_entry where type not like 'rust-cli/%' order by id"),
        ),
    ];
    for (name, mut rows) in tables {
        apply_registered(name, &mut rows);
        let expected = fixture["tables"][name].as_array().unwrap();
        assert_eq!(rows.len(), expected.len(), "{name}: row count");
        for (row, expected) in rows.into_iter().zip(expected) {
            assert_eq!(&Value::from(row), expected, "{name}");
        }
    }
}

/// 同一份记录冷读取回提交的内核消息：结果按提交顺序（B 先于 A）、失败结果读回带标签的模型文本、
/// 内联图片读回原样；三个意图都已结掉。
#[tokio::test]
async fn the_tool_turn_reads_back_as_committed() {
    let t = TestStore::new();
    let committed = replay(&t).await;
    let loaded = t.store.load(&SessionId::new(SESSION)).await.unwrap();
    assert_eq!(loaded.messages, committed);
    assert!(
        loaded.open_operations.is_empty(),
        "{:?}",
        loaded.open_operations
    );
}
