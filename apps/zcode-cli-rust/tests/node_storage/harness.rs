//! An engine over a real `NodeStore` with a scripted model: every turn is a
//! `Read` tool step with reasoning, then the answer.
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use zcode_cli_rust::{app::Engine, contract::*, domain::protocol::Request};
use zcode_cli_state::NodeStore;

/// Ids and times from `offset`, so a restarted runtime never reuses them.
struct Clock(AtomicUsize);
impl RuntimeClock for Clock {
    fn id(&self) -> String {
        let n = self.0.fetch_add(1, Ordering::SeqCst);
        format!("00000000-0000-4000-8000-{n:012}")
    }
    fn now(&self) -> u64 {
        1_790_000_000_000 + self.0.load(Ordering::SeqCst) as u64
    }
}

struct Model {
    calls: AtomicUsize,
    requests: mpsc::UnboundedSender<Vec<Value>>,
}
#[async_trait]
impl ModelPort for Model {
    fn format_properties(&self) -> Value {
        json!({"inputFormat": {"supportsText": true, "supportsImage": true, "supportsVideo": true,
            "supportsAudio": false, "supportsPdf": true}, "outputFormat": {"supportsText": true}})
    }
    async fn complete(
        &self,
        messages: Vec<Value>,
        _: &[Value],
        sink: &EventSink,
        _: &CancellationToken,
    ) -> std::result::Result<ModelOutput, ModelFailure> {
        let compaction = messages.last().is_some_and(|m| {
            m["content"]
                .as_str()
                .is_some_and(|c| c.starts_with("CRITICAL: Respond with TEXT ONLY"))
        });
        // 父会话首轮派生子代理：最后一条是“spawn a child”的用户输入时调用 Agent 工具。
        let spawn = messages.last().and_then(|m| {
            (m["role"] == "user")
                .then(|| m["content"].as_str())
                .flatten()
                .filter(|c| c.starts_with("spawn a"))
                .map(|c| c.contains("background"))
        });
        // 用户要求读图时 Read 读取图片，工具结果带媒体（Node tool result media）。
        let image = messages
            .iter()
            .rev()
            .find(|m| m["role"] == "user" && m.get("_zcode_source").is_none())
            .and_then(|m| m["content"].as_str())
            .is_some_and(|c| c.contains("read the image"));
        let file = if image { "shot.png" } else { "a.ts" };
        // 用户要求写入时调用 Write（build 模式下弹出权限询问）。
        let write = messages
            .iter()
            .rev()
            .find(|m| m["role"] == "user" && m.get("_zcode_source").is_none())
            .and_then(|m| m["content"].as_str())
            .is_some_and(|c| c.contains("write it"));
        self.requests.send(messages).unwrap();
        if let Some(background) = spawn {
            let args = json!({"description": "Look around", "prompt": "Inspect a.ts",
                "subagent_type": "general-purpose", "run_in_background": background});
            let call = json!({"id": "call_agent", "type": "function", "function": {"name": "Agent",
                "arguments": args.to_string()}});
            sink.send(Event::ModelStatus(
                json!({"type": "model_request_started", "querySource": "main_turn",
                "attempt": 1, "requestId": "spawn", "providerId": "p", "modelId": "m"}),
            ))
            .await
            .unwrap();
            sink.send(Event::ModelStatus(json!({"type": "model_request_completed", "querySource": "main_turn",
                "attempt": 1, "requestId": "spawn", "providerId": "p", "modelId": "m", "finishReason": "tool-calls"})))
                .await
                .unwrap();
            return Ok(ModelOutput {
                output_limit: false,
                message: json!({"role": "assistant", "content": "", "tool_calls": [call.clone()],
                    "_zcode_origin": {"provider": "p", "model": "m"}}),
                calls: vec![call],
                usage: json!({}),
            });
        }
        if compaction {
            // 压缩摘要请求：只返回摘要，不计入对话步骤。
            return Ok(ModelOutput {
                output_limit: false,
                message: json!({"role": "assistant",
                    "content": "<analysis>ok</analysis><summary>Fixed the parser.</summary>"}),
                calls: vec![],
                usage: json!({"prompt_tokens": 50, "completion_tokens": 10}),
            });
        }
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let tool_step = n.is_multiple_of(2);
        let status = |kind: &str, extra: Value| {
            let mut status = json!({"type": kind, "querySource": "main_turn", "attempt": 1,
                "requestId": format!("req{n}"), "providerId": "p", "modelId": "m"});
            for (key, value) in extra.as_object().unwrap() {
                status[key] = value.clone();
            }
            Event::ModelStatus(status)
        };
        sink.send(status("model_request_started", json!({})))
            .await
            .unwrap();
        let text = if tool_step { "Let me read." } else { "Done." };
        let mut pieces = vec![(text, false)];
        if tool_step {
            pieces.insert(0, ("Look", true));
        }
        for (piece, reasoning) in pieces {
            sink.send(Event::Text {
                response_id: format!("r{n}"),
                text: piece.into(),
                reasoning,
            })
            .await
            .unwrap();
        }
        let finish = if tool_step { "tool-calls" } else { "stop" };
        let usage = json!({"inputTokens": 100, "outputTokens": 5, "totalTokens": 105});
        sink.send(status(
            "model_request_completed",
            json!({"finishReason": finish, "usage": usage}),
        ))
        .await
        .unwrap();
        // 与 HttpModel 一致：返回的 assistant 带来源模型。
        let mut message = json!({"role": "assistant", "content": text,
            "_zcode_origin": {"provider": "p", "model": "m"}});
        let mut calls = vec![];
        if tool_step {
            message["reasoning_content"] = "Look".into();
            calls = vec![json!({"id": format!("call_{n}"), "type": "function",
            "function": if write {
                json!({"name": "Write", "arguments": json!({"file_path": "b.ts", "content": "x"}).to_string()})
            } else {
                json!({"name": "Read", "arguments": json!({"file_path": file}).to_string()})
            }})];
            message["tool_calls"] = json!(calls);
        }
        Ok(ModelOutput {
            output_limit: false,
            message,
            calls,
            usage: json!({"prompt_tokens": 100, "completion_tokens": 5}),
        })
    }
}

/// The image `Read` returns for `shot.png`.
pub const IMAGE: &[u8] = &[137, 80, 78, 71, 13, 10, 26, 10, 1, 2, 3];

/// `Read`; the first execution waits for the gate when one is set.
pub struct Tools {
    gate: Mutex<Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>>,
    root: PathBuf,
}
#[async_trait]
impl ToolPort for Tools {
    fn definitions(&self) -> Vec<Value> {
        vec![
            json!({"type": "function", "function": {"name": "Read", "parameters": {"type": "object"}}}),
            json!({"type": "function", "function": {"name": "Agent", "parameters": {"type": "object"}}}),
            json!({"type": "function", "function": {"name": "Write", "parameters": {"type": "object"}}}),
        ]
    }
    fn concurrent_safe(&self, _: &str) -> bool {
        true
    }
    async fn agent_output(&self, session: &str, _: &str) -> Result<String> {
        Ok(format!("agent-output/{session}.md"))
    }
    async fn execute_scoped(
        &self,
        name: &str,
        arguments: &Value,
        _: &EventSink,
        cancel: &CancellationToken,
    ) -> Result<ToolOutput> {
        if arguments["file_path"] != "shot.png" {
            return Ok(ToolOutput::text(
                self.execute(name, arguments, cancel).await?,
            ));
        }
        let path = self.root.join("read-media.png");
        std::fs::write(&path, IMAGE)?;
        let asset = json!({"path": path, "mediaType": "image/png", "totalBytes": IMAGE.len()});
        let mut output = ToolOutput::new(
            "[Attached image/png: Read image]".into(),
            json!({"type": "image"}),
        );
        output.model_content = Some(json!([{"type": "_zcode_attachment", "asset": asset,
            "name": "shot.png", "placeholder": "Read image", "sizeBytes": IMAGE.len()}]));
        Ok(output)
    }
    async fn execute(&self, _: &str, _: &Value, _: &CancellationToken) -> Result<String> {
        let gate = self.gate.lock().unwrap().take();
        if let Some((started, release)) = gate {
            started.send(()).unwrap();
            release.await?;
        }
        Ok("1\tconst a = 1;".into())
    }
}

pub struct Harness {
    input: mpsc::Sender<Input>,
    output: mpsc::Receiver<Vec<Value>>,
    pub requests: mpsc::UnboundedReceiver<Vec<Value>>,
    pub db: PathBuf,
    pub workspace: String,
    pub root: PathBuf,
    _dir: Option<Arc<tempfile::TempDir>>,
}

/// A runtime in a fresh root: `ZCODE_CLI_RUST_NODE_DUMP/<dump>` when set
/// (kept for `scripts/zcode-cli-rust-node-storage-check.mjs`), else a
/// temporary one.
pub async fn start(
    dump: Option<&str>,
    gate: Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>,
) -> Harness {
    let target = std::env::var_os("ZCODE_CLI_RUST_NODE_DUMP").zip(dump);
    let (root, dir) = match target {
        Some((root, name)) => (PathBuf::from(root).join(name), None),
        None => {
            let dir = tempfile::tempdir().unwrap();
            (dir.path().to_path_buf(), Some(Arc::new(dir)))
        }
    };
    run(root, dir, 0, gate).await
}

/// Another runtime over the same database, as after a restart.
pub async fn restart(previous: &Harness) -> Harness {
    run(previous.root.clone(), previous._dir.clone(), 10_000, None).await
}

async fn run(
    root: PathBuf,
    dir: Option<Arc<tempfile::TempDir>>,
    offset: usize,
    gate: Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>,
) -> Harness {
    let db = root.join("cli/db/db.sqlite");
    let workspace = root.join("w").to_string_lossy().into_owned();
    let store = NodeStore::open(db.clone(), root.join("cli/artifacts"))
        .await
        .unwrap();
    let (requests_tx, requests) = mpsc::unbounded_channel();
    let ports = RuntimePorts {
        context: Arc::new(zcode_cli_host::context_source::WorkspaceContext::new(
            root.clone(),
            root.join("home"),
            false,
            vec![].into(),
        )),
        store: Arc::new(store),
        model: Some(Arc::new(Model {
            calls: AtomicUsize::new(0),
            requests: requests_tx,
        })),
        tools: Arc::new(Tools {
            gate: Mutex::new(gate),
            root: root.clone(),
        }),
        clock: Arc::new(Clock(AtomicUsize::new(offset))),
    };
    let identity = ModelIdentity {
        provider_id: "p".into(),
        model_id: "m".into(),
        reasoning_level: "high".into(),
    };
    let engine = Engine::new(workspace.clone(), Some(identity), ports)
        .await
        .unwrap();
    let (input, rx) = mpsc::channel(32);
    let (out, output) = mpsc::channel(256);
    tokio::spawn(zcode_cli_rust::serve_values(
        engine,
        rx,
        out,
        CancellationToken::new(),
    ));
    Harness {
        input,
        output,
        requests,
        db,
        workspace,
        root,
        _dir: dir,
    }
}

impl Harness {
    /// Sends one V4 command and returns its acknowledgement.
    pub async fn command(&mut self, id: u64, params: Value) -> Value {
        self.request(id, "v4/command", params).await
    }

    /// The live rows of `session` with the revision and log epoch they belong to.
    pub async fn rows(&mut self, id: u64, session: &str) -> (Vec<Value>, Value, Value) {
        let page = self
            .request(
                id,
                "v4/conversation/rowsRange",
                json!({"sessionId": session, "limit": 200}),
            )
            .await;
        let rows = page["rows"].as_array().cloned().expect("rows");
        (rows, page["atRevision"].clone(), page["atLogEpoch"].clone())
    }

    /// Sends one request and returns its result.
    pub async fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        let request: Request =
            serde_json::from_value(json!({"id": id, "method": method, "params": params})).unwrap();
        self.input.send(Input::Request(request)).await.unwrap();
        loop {
            let frames =
                tokio::time::timeout(std::time::Duration::from_secs(5), self.output.recv())
                    .await
                    .expect("response in time")
                    .unwrap();
            if let Some(frame) = frames.into_iter().find(|f| f["id"] == id) {
                return frame["result"].clone();
            }
        }
    }

    /// Starts a session with `text` as its first input.
    pub async fn create(&mut self, command: &str, text: &str) -> String {
        self.create_in("yolo", command, text).await
    }

    /// [`Harness::create`] in execution `mode`.
    pub async fn create_in(&mut self, mode: &str, command: &str, text: &str) -> String {
        let workspace = self.workspace.clone();
        let ack = self
            .command(
                1,
                json!({"commandId": command, "clientId": "cli", "sessionId": null,
                "type": "createSession", "issuedAt": 1, "payload": {"workspaceId": workspace,
                    "config": {"mode": mode}, "firstInput": {"text": text}}}),
            )
            .await;
        ack["result"]["sessionId"]
            .as_str()
            .expect("session id")
            .to_owned()
    }

    pub async fn send_text(&mut self, id: u64, session: &str, command: &str, text: &str) -> Value {
        self.command(
            id,
            json!({"commandId": command, "clientId": "cli", "sessionId": session,
                "type": "sendText", "issuedAt": 1, "payload": {"text": text}}),
        )
        .await
    }

    /// Waits until `turns` turns of the session completed with their boundary.
    pub async fn settled(&self, session: &str, turns: u32) -> rusqlite::Connection {
        let conn = rusqlite::Connection::open(&self.db).unwrap();
        for _ in 0..250 {
            let done: u32 = conn
                .query_row(
                    "select count(*) from message where session_id = ?
                     and json_extract(data, '$.anchor.boundaryMessageId') is not null",
                    [session],
                    |r| r.get(0),
                )
                .unwrap();
            if done == turns {
                return conn;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("turns not persisted");
    }
}

/// Writes what Rust reads back of `session` (history entries, rows and
/// state) next to the kept database, for Node's reading check.
pub fn dump(h: &Harness, conn: &rusqlite::Connection, session: &str) {
    dump_as(h, conn, session, "rust.json");
}

/// [`dump`] under another file name (several sessions of one database).
pub fn dump_as(h: &Harness, conn: &rusqlite::Connection, session: &str, file: &str) {
    if h._dir.is_some() {
        return;
    }
    let active = zcode_cli_state::node::cold::active(conn, session).unwrap();
    let artifacts = h.root.join("cli/artifacts");
    let read = |uri: &str| zcode_cli_state::node::artifacts::read(&artifacts, uri);
    let history: Vec<Value> = zcode_cli_rust::domain::node_history::hydrate(&active, &read)
        .entries
        .iter()
        .map(|e| e.to_node())
        .collect();
    let resumed = zcode_cli_state::node::resume::resume(conn, session, &read, None)
        .unwrap()
        .unwrap();
    let read = json!({"sessionId": session, "history": history,
        "rows": resumed.conversation.rows, "state": resumed.conversation.state});
    std::fs::write(h.root.join(file), read.to_string()).unwrap();
}
