//! json / stream-json 的输出与 stream-json 的输入（docs/specs/rust-cli/headless.md §3、§4，修复线 F39）：内存里的
//! 内核与脚本模型，核对事件的种类、顺序与键序。

use std::sync::Arc;

use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::{
    FixedClock, MemoryStore, ScriptedModel, SequentialIds, UnusedEnvironment, environments,
    selection, text_response, tool_use_response,
};
use rust_cli_runtime::{Defaults, LocalKernel, Ports};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::*;

fn kernel(model: ScriptedModel, max_turns: Option<u32>) -> Arc<dyn Kernel> {
    let store = Arc::new(MemoryStore::default());
    Arc::new(LocalKernel::new(
        Ports {
            model: Arc::new(model),
            environments: environments(Arc::new(UnusedEnvironment)),
            store: store.clone(),
            artifacts: store,
            clock: Arc::new(FixedClock(1_767_225_600_000)),
            ids: Arc::new(SequentialIds::default()),
        },
        Defaults {
            selection: selection(),
            mode: PermissionMode::Default,
            max_turns,
            search_tools: false,
            task_tools: None,
            limits: Default::default(),
        },
    ))
}

fn options(prompt: &str, output: OutputFormat, verbose: bool) -> PrintOptions {
    PrintOptions {
        prompt: prompt.to_owned(),
        session_id: None,
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/work"),
        },
        selection: selection(),
        mode: PermissionMode::Default,
        target: SessionTarget::New,
        fork: false,
        stop: CancellationToken::new(),
        cancel: CancellationToken::new(),
        output: Output {
            format: PrintFormat {
                output,
                verbose,
                partial: false,
            },
            stamps: Some(Stamps {
                ids: Arc::new(SequentialIds::default()),
                clock: Arc::new(FixedClock(1_767_225_600_000)),
            }),
            version: "0.1.0".to_owned(),
            api_key_source: "none".to_owned(),
        },
    }
}

fn events(output: &[u8]) -> Vec<Value> {
    String::from_utf8(output.to_vec())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn kinds(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .map(|event| {
            let kind = event["type"].as_str().unwrap_or_default().to_owned();
            match (event["subtype"].as_str(), event["event"]["type"].as_str()) {
                (Some(subtype), _) => format!("{kind}/{subtype}"),
                (_, Some(inner)) => format!("{kind}/{inner}"),
                _ => kind,
            }
        })
        .collect()
}

fn keys(value: &Value) -> Vec<&str> {
    value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect()
}

fn read_then_answer() -> ScriptedModel {
    ScriptedModel::new(vec![
        tool_use_response(
            "toolu_1",
            "Read",
            &serde_json::json!({"file_path": "/work/a"}),
        ),
        text_response(&["Done", "."]),
    ])
}

/// stream-json：init、每个块一个 assistant、工具结果的 user、result；result 的键序固定（去掉 D14 的键）。
#[tokio::test]
async fn stream_json_writes_the_events_in_key_order() {
    let mut output = Vec::new();
    run(
        kernel(read_then_answer(), None),
        options("go", OutputFormat::StreamJson, true),
        &mut output,
    )
    .await
    .unwrap();
    let events = events(&output);
    assert_eq!(
        kinds(&events),
        [
            "system/init",
            "assistant",
            "user",
            "assistant",
            "result/success"
        ]
    );
    let init = &events[0];
    assert_eq!(
        keys(init),
        [
            "type",
            "subtype",
            "cwd",
            "session_id",
            "tools",
            "mcp_servers",
            "model",
            "permissionMode",
            "slash_commands",
            "apiKeySource",
            "zcode_version",
            "output_style",
            "agents",
            "skills",
            "uuid"
        ]
    );
    assert_eq!(init["cwd"], "/work");
    assert_eq!(init["permissionMode"], "default");
    assert_eq!(init["model"], "scripted");
    let tools: Vec<&str> = init["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool.as_str().unwrap())
        .collect();
    // Agent 写成 Task；按名字排序。
    assert!(
        tools.contains(&"Task") && !tools.contains(&"Agent"),
        "{tools:?}"
    );
    assert!(tools.contains(&"Read"));
    let assistant = &events[1];
    assert_eq!(
        keys(assistant),
        [
            "type",
            "message",
            "parent_tool_use_id",
            "session_id",
            "uuid",
            "timestamp"
        ]
    );
    assert_eq!(assistant["message"]["stop_reason"], Value::Null);
    assert_eq!(assistant["message"]["content"][0]["type"], "tool_use");
    assert_eq!(assistant["timestamp"], "2026-01-01T00:00:00.000Z");
    let user = &events[2];
    let block = &user["message"]["content"][0];
    assert_eq!(block["tool_use_id"], "toolu_1");
    // 失败的工具结果：键序 `type`、`content`、`is_error`、`tool_use_id`，正文是字符串。
    assert_eq!(keys(block), ["type", "content", "is_error", "tool_use_id"]);
    assert!(block["content"].is_string());
    let result = &events[4];
    assert_eq!(
        keys(result),
        [
            "duration_api_ms",
            "stop_reason",
            "session_id",
            "total_cost_usd",
            "usage",
            "modelUsage",
            "permission_denials",
            "terminal_reason",
            "is_error",
            "num_turns",
            "subtype",
            "api_error_status",
            "result",
            "ttft_ms",
            "type",
            "duration_ms",
            "uuid",
            "queued_turn_count",
            "result_index"
        ]
    );
    assert_eq!(result["result"], "Done.");
    assert_eq!(result["num_turns"], 2);
    assert_eq!(result["stop_reason"], "end_turn");
    assert_eq!(result["terminal_reason"], "completed");
    assert_eq!(result["total_cost_usd"], 0);
    assert_eq!(result["session_id"], init["session_id"]);
    assert_eq!(result["result_index"], 0);
}

/// json：只写最后一个 result（一行）；`--verbose` 时是全部事件的数组。
#[tokio::test]
async fn json_writes_the_last_result_or_every_event() {
    let mut output = Vec::new();
    run(
        kernel(read_then_answer(), None),
        options("go", OutputFormat::Json, false),
        &mut output,
    )
    .await
    .unwrap();
    let text = String::from_utf8(output).unwrap();
    assert_eq!(text.lines().count(), 1);
    let result: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(result["type"], "result");
    assert_eq!(result["result"], "Done.");
    let mut output = Vec::new();
    run(
        kernel(read_then_answer(), None),
        options("go", OutputFormat::Json, true),
        &mut output,
    )
    .await
    .unwrap();
    let all: Value = serde_json::from_slice(&output).unwrap();
    let all = all.as_array().unwrap().clone();
    assert_eq!(
        kinds(&all),
        [
            "system/init",
            "assistant",
            "user",
            "assistant",
            "result/success"
        ]
    );
}

/// 轮数用尽：`error_max_turns`（`errors` 是固定的原文，`num_turns` 是 turnCount）；json / stream-json 返回
/// `ResultIsError`（入口不另写 stderr、退出码 1）。
#[tokio::test]
async fn max_turns_ends_with_error_max_turns() {
    let mut output = Vec::new();
    let error = run(
        kernel(read_then_answer(), Some(1)),
        options("go", OutputFormat::StreamJson, true),
        &mut output,
    )
    .await
    .unwrap_err();
    assert!(error.is::<ResultIsError>(), "{error}");
    let events = events(&output);
    let result = events.last().unwrap();
    assert_eq!(result["subtype"], "error_max_turns");
    assert_eq!(result["is_error"], true);
    assert_eq!(result["num_turns"], 2);
    assert_eq!(result["terminal_reason"], "max_turns");
    assert_eq!(result["stop_reason"], "tool_use");
    assert_eq!(result["errors"][0], "Reached maximum number of turns (1)");
    assert!(result.get("result").is_none() && result.get("api_error_status").is_none());
}

/// stream-json 输入：一行一个回合（其余行忽略），每个回合前一个 init；`result_index` 递增；同一个会话。
#[tokio::test]
async fn stream_input_runs_one_turn_per_line() {
    let kernel = kernel(
        ScriptedModel::new(vec![text_response(&["one"]), text_response(&["two"])]),
        None,
    );
    let input = concat!(
        r#"{"type":"user","message":{"role":"user","content":"first"}}"#,
        "\n\n",
        r#"{"type":"assistant","message":{"role":"assistant","content":"x"}}"#,
        "\n",
        r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"second"}]}}"#,
        "\n"
    );
    let mut output = Vec::new();
    run_stream(
        kernel.clone(),
        options("", OutputFormat::StreamJson, true),
        input.as_bytes(),
        &mut output,
    )
    .await
    .unwrap();
    let events = events(&output);
    assert_eq!(
        kinds(&events),
        [
            "system/init",
            "assistant",
            "result/success",
            "system/init",
            "assistant",
            "result/success"
        ]
    );
    assert_eq!(events[2]["result"], "one");
    assert_eq!(events[5]["result"], "two");
    assert_eq!(events[5]["result_index"], 1);
    assert_eq!(events[0]["session_id"], events[3]["session_id"]);
    // 一个会话（一行一次 `create_session` 的话会有两个）。
    let sessions = kernel
        .list_sessions(rust_cli_kernel_api::store::SessionFilter {
            workspace: options("", OutputFormat::StreamJson, true).workspace,
            limit: 10,
        })
        .await
        .unwrap();
    assert_eq!(sessions.len(), 1);
}

/// 一行都没有：不建会话、什么都不写。不是 JSON 的行：报错的原文，之后的行不读。
#[tokio::test]
async fn empty_or_malformed_streams() {
    let kernel = kernel(ScriptedModel::new(Vec::new()), None);
    let mut output = Vec::new();
    run_stream(
        kernel.clone(),
        options("", OutputFormat::StreamJson, true),
        "".as_bytes(),
        &mut output,
    )
    .await
    .unwrap();
    assert!(output.is_empty());
    let error = run_stream(
        kernel,
        options("", OutputFormat::StreamJson, true),
        "not json\n{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"x\"}}\n"
            .as_bytes(),
        &mut output,
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.downcast_ref::<InputError>(),
        Some(&InputError(
            "Error parsing streaming input line (type=unknown, 8 chars): SyntaxError".to_owned()
        ))
    );
    assert!(output.is_empty());
}

/// `--include-partial-messages`：文字的增量包成 Anthropic 的流事件；消息完成时先写 assistant，再写块的结束与
/// `message_delta`、`message_stop`（单块的消息顺序同此，headless.md §3.7）。
#[tokio::test]
async fn partial_messages_wrap_the_stream() {
    let mut options = options("go", OutputFormat::StreamJson, true);
    options.output.format.partial = true;
    let mut output = Vec::new();
    run(
        kernel(
            ScriptedModel::new(vec![text_response(&["Hel", "lo"])]),
            None,
        ),
        options,
        &mut output,
    )
    .await
    .unwrap();
    let events = events(&output);
    assert_eq!(
        kinds(&events),
        [
            "system/init",
            "stream_event/message_start",
            "stream_event/content_block_start",
            "stream_event/content_block_delta",
            "stream_event/content_block_delta",
            "assistant",
            "stream_event/content_block_stop",
            "stream_event/message_delta",
            "stream_event/message_stop",
            "result/success"
        ]
    );
    assert_eq!(events[3]["event"]["delta"]["text"], "Hel");
    assert_eq!(events[7]["event"]["delta"]["stop_reason"], "end_turn");
    assert_eq!(
        keys(&events[1]),
        ["type", "event", "session_id", "parent_tool_use_id", "uuid"]
    );
}

/// 本地命令（`/usage`）：stream-json 是 init、合成的 assistant 与 `num_turns: 0` 的 result（本地命令的键序）；
/// text 仍是命令的文字。
#[tokio::test]
async fn local_commands_write_a_synthetic_reply() {
    let mut output = Vec::new();
    run(
        kernel(ScriptedModel::new(Vec::new()), None),
        options("/usage", OutputFormat::StreamJson, true),
        &mut output,
    )
    .await
    .unwrap();
    let events = events(&output);
    assert_eq!(
        kinds(&events),
        ["system/init", "assistant", "result/success"]
    );
    assert_eq!(events[1]["message"]["model"], "<synthetic>");
    let result = &events[2];
    assert_eq!(result["num_turns"], 0);
    assert_eq!(result["local_command"], "usage");
    assert_eq!(
        &keys(result)[..3],
        ["is_error", "duration_api_ms", "num_turns"]
    );
    assert!(
        result["result"]
            .as_str()
            .unwrap()
            .starts_with("Total duration (API)")
    );
}
