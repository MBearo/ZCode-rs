//! 修复线 F101（docs/specs/rust-cli/model.md §10.3「函数调用的参数」）：OpenAI Responses 的供应商不发参数增量，
//! 完整参数只在 `function_call_arguments.done` 与 `output_item.done` 里。一回合之后工具拿到完整的参数、执行成功，
//! 库里 tool part 的 input 也是完整的参数。
//!
//! 在进程内照入口的写法组装内核：真实的模型适配器（解码线上的流）、主循环、会话 actor、env-local 与 node-store
//! （临时 HOME 下的 db.sqlite），模型请求发到本地 SSE 夹具。
//!
//! 修 bug 前 `output_item.done` 只结束块、不读 `item.arguments`：4 个并行调用的 input 都是 `{}`，全部报
//! `InputValidationError`（必填参数缺失），库里的 input 也是 `{}`。
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "集成测试：准备临时目录、起本地 HTTP 夹具、读回会话与写出的文件"
)]

mod sse;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use rust_cli_base::secret::RedactedString;
use rust_cli_env_local::LocalEnvironment;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{CommandId, EnvironmentId, SessionId};
use rust_cli_kernel_api::message::{ContentBlock, Message};
use rust_cli_kernel_api::model::{ModelPort, ModelSelection};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::provider::{Credential, ModelEndpoint, WireProtocol};
use rust_cli_kernel_api::session_api::{
    Ack, Command, CommandEnvelope, CreateSession, Delivery, Kernel, KernelEvent,
};
use rust_cli_kernel_api::store::SessionStore;
use rust_cli_kernel_api::testing::environments;
use rust_cli_model::{FixedEndpoint, ProviderModel};
use rust_cli_net::{Egress, NetworkPolicy};
use rust_cli_node_store::{NodeIds, NodeStore, SystemClock};
use rust_cli_paths::{StoragePaths, StorageSettings};
use rust_cli_runtime::{Defaults, LocalKernel, Ports};
use serde_json::{Value, json};

fn event(kind: &str, data: Value) -> String {
    sse::raw_event(kind, data)
}

fn created() -> String {
    event(
        "response.created",
        json!({"response": {"id": "resp_1", "model": "gpt-test", "status": "in_progress"}}),
    )
}

fn completed() -> String {
    event(
        "response.completed",
        json!({"response": {"id": "resp_1", "status": "completed",
            "usage": {"input_tokens": 100, "output_tokens": 20, "input_tokens_details": {"cached_tokens": 0}}}}),
    )
}

/// 并行的函数调用，每个都是 added（`arguments` 为空）→ `function_call_arguments.done` → `output_item.done`，
/// 没有 `function_call_arguments.delta`。
fn calls_reply(calls: &[(&str, &str, Value)]) -> String {
    let mut reply = created();
    for (index, (call_id, name, arguments)) in calls.iter().enumerate() {
        let arguments = arguments.to_string();
        let item = |arguments: &str| {
            json!({"type": "function_call", "id": format!("fc_{index}"), "call_id": call_id,
                "name": name, "arguments": arguments})
        };
        let mut done = item(&arguments);
        done["status"] = json!("completed");
        reply += &event(
            "response.output_item.added",
            json!({"output_index": index, "item": item("")}),
        );
        reply += &event(
            "response.function_call_arguments.done",
            json!({"output_index": index, "item_id": format!("fc_{index}"), "arguments": arguments}),
        );
        reply += &event(
            "response.output_item.done",
            json!({"output_index": index, "item": done}),
        );
    }
    reply + &completed()
}

fn text_reply(text: &str) -> String {
    created()
        + &event(
            "response.output_item.added",
            json!({"output_index": 0, "item": {"type": "message", "id": "msg_1", "role": "assistant"}}),
        )
        + &event(
            "response.output_text.delta",
            json!({"output_index": 0, "item_id": "msg_1", "delta": text}),
        )
        + &event(
            "response.output_item.done",
            json!({"output_index": 0, "item": {"type": "message", "id": "msg_1"}}),
        )
        + &completed()
}

fn endpoint(base_url: String) -> ModelEndpoint {
    ModelEndpoint {
        protocol: WireProtocol::OpenAiResponses,
        base_url,
        model: String::new(),
        credential: Credential::ApiKey(RedactedString::new("sk-test")),
        headers: Vec::new(),
        body_patches: Vec::new(),
        mid_conversation_system: true,
        supports_image: true,
        supports_pdf: false,
        supports_video: false,
        supports_audio: false,
        renewable: false,
    }
}

fn selection() -> ModelSelection {
    ModelSelection {
        provider: "openai-test".to_owned(),
        model: "gpt-test".to_owned(),
        reasoning: None,
        connection: None,
    }
}

/// 一次运行的临时 home 与工作区。
struct Sandbox {
    _dir: tempfile::TempDir,
    home: PathBuf,
    work: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let (home, work) = (base.join("home"), base.join("work"));
        std::fs::create_dir_all(work.join(".git")).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        Self {
            _dir: dir,
            home,
            work,
        }
    }

    fn path(&self, name: &str) -> String {
        self.work.join(name).to_string_lossy().into_owned()
    }

    fn store(&self) -> Arc<NodeStore> {
        let paths = StoragePaths::resolve(&StorageSettings::default(), &self.home, &self.work);
        let ids = Arc::new(NodeIds::new(Arc::new(SystemClock::utc())));
        Arc::new(NodeStore::new(paths, ids))
    }

    /// 照入口组装内核：模型适配器连夹具（固定端点），执行端口是本机的 env-local，存储是临时 HOME 下的 node-store。
    async fn kernel(&self, endpoint: ModelEndpoint) -> LocalKernel {
        let clock = Arc::new(SystemClock::utc());
        let egress = Arc::new(Egress::new(NetworkPolicy::default()).await.unwrap());
        let model: Arc<dyn ModelPort> = Arc::new(ProviderModel::new(
            egress,
            Arc::new(FixedEndpoint::new(endpoint)),
            clock.clone(),
        ));
        let store = self.store();
        let ports = Ports {
            model,
            environments: environments(Arc::new(LocalEnvironment::new(Some(self.home.clone())))),
            store: store.clone(),
            artifacts: store,
            ids: Arc::new(NodeIds::new(clock.clone())),
            clock,
        };
        let defaults = Defaults {
            selection: selection(),
            mode: PermissionMode::BypassPermissions,
            max_turns: None,
            search_tools: false,
            task_tools: None,
            limits: Default::default(),
        };
        LocalKernel::new(ports, defaults)
    }

    /// 开一个会话、提交一条输入，等回合结束。
    async fn one_turn(&self, kernel: &LocalKernel) -> SessionId {
        let id = kernel
            .create_session(CreateSession {
                id: None,
                workspace: WorkspaceRef {
                    environment: EnvironmentId::new("local"),
                    root: EnvPath::new(self.work.to_string_lossy()),
                },
                selection: selection(),
                mode: PermissionMode::BypassPermissions,
                plan: false,
            })
            .await
            .unwrap();
        let handle = kernel.open_session(&id).await.unwrap();
        let mut events = handle.subscribe(Some(0));
        let ack = handle
            .send(CommandEnvelope {
                id: CommandId::new("cmd_calls"),
                command: Command::Submit {
                    text: "look at the notes".to_owned(),
                    attachments: Vec::new(),
                    delivery: Delivery::Queue,
                },
            })
            .await;
        assert!(!matches!(ack, Ack::Rejected { .. }), "{ack:?}");
        // 期限只是测试的看门狗：等的是回合结束的事件，不靠时间对齐状态。
        tokio::time::timeout(Duration::from_secs(60), async {
            while let Some(event) = events.next().await {
                if matches!(event.event, KernelEvent::TurnFinished { .. }) {
                    return;
                }
            }
            panic!("the event stream ended before the turn end");
        })
        .await
        .expect("the turn finishes");
        id
    }
}

/// 请求里 `function_call_output` 的 call_id → 输出。
fn outputs(request: &Value) -> BTreeMap<String, String> {
    request["input"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| item["type"] == "function_call_output")
        .map(|item| {
            (
                item["call_id"].as_str().unwrap().to_owned(),
                item["output"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn parallel_calls_without_argument_deltas_run_with_their_arguments() {
    let sandbox = Sandbox::new();
    std::fs::write(sandbox.work.join("notes.txt"), "first note\n").unwrap();
    std::fs::write(sandbox.work.join("todo.md"), "- 待办：整理笔记\n").unwrap();
    let calls = [
        (
            "call_read",
            "Read",
            json!({"file_path": sandbox.path("notes.txt")}),
        ),
        (
            "call_write",
            "Write",
            json!({"file_path": sandbox.path("out.txt"), "content": "中文参数：第一行\n第二行\n"}),
        ),
        (
            "call_plan",
            "Write",
            json!({"file_path": sandbox.path("plan.md"), "content": "计划"}),
        ),
        (
            "call_todo",
            "Read",
            json!({"file_path": sandbox.path("todo.md")}),
        ),
    ];
    let fixture = sse::serve(vec![calls_reply(&calls), text_reply("Done.")]);
    let kernel = sandbox.kernel(endpoint(fixture.url.clone())).await;
    let session = sandbox.one_turn(&kernel).await;

    // 工具拿到完整的参数并执行成功：第二个请求里的结果都不是校验错误，写出的文件内容就是参数里的中文。
    let requests = fixture.main_requests();
    assert_eq!(requests.len(), 2);
    let outputs = outputs(&requests[1]);
    assert_eq!(outputs.len(), calls.len(), "{outputs:?}");
    for (id, output) in &outputs {
        assert!(!output.contains("InputValidationError"), "{id}: {output}");
    }
    for (id, part) in [("call_read", "first note"), ("call_todo", "待办：整理笔记")] {
        assert!(outputs[id].contains(part), "{id}: {}", outputs[id]);
    }
    let written = |name: &str| std::fs::read_to_string(sandbox.work.join(name)).unwrap();
    assert_eq!(written("out.txt"), "中文参数：第一行\n第二行\n");
    assert_eq!(written("plan.md"), "计划");
    // 回放给模型的调用带着完整的参数。
    let replayed: Vec<Value> = requests[1]["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["type"] == "function_call")
        .map(|item| serde_json::from_str(item["arguments"].as_str().unwrap()).unwrap())
        .collect();
    let expected: Vec<Value> = calls.iter().map(|(_, _, input)| input.clone()).collect();
    assert_eq!(replayed, expected);

    // 冷读取（经 node-store 从 db.sqlite 重建）：tool part 的 input 是完整的参数，结果都不是错误。
    let loaded = sandbox.store().load(&session).await.unwrap();
    let mut stored = Vec::new();
    let mut results = Vec::new();
    for message in &loaded.messages {
        let content = match message {
            Message::Assistant(message) => &message.content,
            Message::User(message) => &message.content,
            _ => continue,
        };
        for block in content {
            match block {
                ContentBlock::ToolUse { id, name, input } => {
                    stored.push((id.as_str().to_owned(), name.clone(), input.clone()));
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    is_error,
                    ..
                } => results.push((tool_use_id.as_str().to_owned(), *is_error)),
                _ => {}
            }
        }
    }
    let expected: Vec<_> = calls
        .iter()
        .map(|(id, name, input)| ((*id).to_owned(), (*name).to_owned(), input.clone()))
        .collect();
    assert_eq!(stored, expected);
    // 每个调用一个结果，都不是错误（这里不比结果的先后）。
    results.sort();
    let mut succeeded: Vec<_> = calls
        .iter()
        .map(|(id, ..)| ((*id).to_owned(), false))
        .collect();
    succeeded.sort();
    assert_eq!(results, succeeded);
}
