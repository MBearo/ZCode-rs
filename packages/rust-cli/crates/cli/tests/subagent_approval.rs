//! L34 的复现（docs/specs/rust-cli/README.md §11 L34）：主线程同一步派两个 Agent——前台 A 先跑 Bash（要审批，
//! 用户选“允许并记住 `mkdir:*`”），再 Write / Edit / Write（notebook）/ NotebookEdit；后台 B 的写入被拒绝（交互界面里
//! 后台子代理也询问，F67 起由测试扮演的用户拒绝；以前是没人回答、自动拒绝）。
//!
//! 真实的内核组装：LocalKernel（主循环、agents、执行管线、PolicyGate 的 default 模式）、真实 env-local、真实
//! node-store（临时 HOME 下的 db.sqlite 与 artifacts）。模型按请求来源与 prompt 里的标记分流。超时只用于测试判定。
//!
//! 1. 批准并记住之后，A 继续发下一个模型请求，写入都成功（没有复现卡住）。
//! 2. 工作区放在 `CARGO_TARGET_TMPDIR` 下：本机上它和系统临时目录在不同的卷上（外接盘上的仓库），写入与写前
//!    checkpoint 都不经临时目录（env-local 原地写；artifact 的临时文件与目标同目录），跨卷不报错。
//! 3. A 的第二个模型请求开着、一直没有完成的块（服务端慢、在重试或流停住），用户中断：子会话的记录与用户 db 里
//!    的事故记录相同（Bash 结果之后直接是 `[Request interrupted by user]`）。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stderr,
    reason = "集成测试：准备临时目录、检查写出的文件、打印诊断"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use futures_util::stream::{self, BoxStream};
use rust_cli_env_local::{LocalEnvironment, ShellConfig};
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{CommandId, EnvironmentId, ToolUseId};
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::message::{ContentBlock, StopReason, ToolResultContent, Usage};
use rust_cli_kernel_api::model::{
    BlockDelta, ModelError, ModelEvent, ModelPort, ModelRequest, QuerySource,
};
use rust_cli_kernel_api::orchestration::InteractionRequest;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{
    Command, CommandEnvelope, CreateSession, Delivery, Kernel, KernelEvent, SessionHandle,
};
use rust_cli_kernel_api::store::SessionStore;
use rust_cli_kernel_api::testing::{ScriptedModel, environments, selection, text_response};
use rust_cli_node_store::NodeStore;
use rust_cli_node_store::{NodeIds, SystemClock};
use rust_cli_paths::{StoragePaths, StorageSettings};
use rust_cli_runtime::{Defaults, LocalKernel, Ports};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

const MARK_A: &str = "L34-AGENT-A";
const MARK_B: &str = "L34-AGENT-B";

type Events = Vec<Result<ModelEvent, ModelError>>;

/// nbformat 4.4 的最小 notebook（没有单元格 ID，`cell-0` 按下标）。
const NOTEBOOK: &str = r#"{"cells": [{"cell_type": "code", "execution_count": null, "metadata": {}, "outputs": [], "source": ["print(1)"]}], "metadata": {}, "nbformat": 4, "nbformat_minor": 4}"#;

/// 一次响应里的多个 tool_use 块（主线程同一步派两个 Agent）。
fn tools_response(calls: &[(&str, &str, Value)]) -> Events {
    let mut events = vec![Ok(ModelEvent::MessageStart {
        id: "msg_api".to_owned(),
        model: "scripted".to_owned(),
        usage: Usage::default(),
    })];
    for (index, (id, name, input)) in calls.iter().enumerate() {
        events.extend([
            Ok(ModelEvent::BlockStart {
                index,
                block: ContentBlock::ToolUse {
                    id: ToolUseId::new(*id),
                    name: (*name).to_owned(),
                    input: json!({}),
                },
            }),
            Ok(ModelEvent::BlockDelta {
                index,
                delta: BlockDelta::InputJson(input.to_string()),
            }),
            Ok(ModelEvent::BlockStop { index }),
        ]);
    }
    events.extend([
        Ok(ModelEvent::MessageDelta {
            stop_reason: Some(StopReason::ToolUse),
            usage: Usage::default(),
        }),
        Ok(ModelEvent::MessageStop),
    ]);
    events
}

fn one(id: &str, name: &str, input: Value) -> Events {
    tools_response(&[(id, name, input)])
}

/// 主线程按剧本；子代理按 prompt 里的标记分给 A / B；其余（标题等旁路请求）回一段文字。
struct Routed {
    main: ScriptedModel,
    a: ScriptedModel,
    b: ScriptedModel,
    side: ScriptedModel,
    /// A 的第几个请求（从 0 数）只给出推理的开头、之后一直不结束（服务端慢或卡住）；取消时流结束。
    a_stall_at: Option<usize>,
}

/// 只有 `MessageStart` 与一段推理，之后不再有事件，直到取消。
fn stalled(cancel: CancellationToken) -> BoxStream<'static, Result<ModelEvent, ModelError>> {
    let head = vec![
        Ok(ModelEvent::MessageStart {
            id: "msg_api".to_owned(),
            model: "scripted".to_owned(),
            usage: Usage::default(),
        }),
        Ok(ModelEvent::BlockStart {
            index: 0,
            block: ContentBlock::Thinking {
                thinking: String::new(),
                signature: None,
            },
        }),
        Ok(ModelEvent::BlockDelta {
            index: 0,
            delta: BlockDelta::Thinking("Planning the whole page".to_owned()),
        }),
    ];
    stream::iter(head)
        .chain(stream::pending())
        .take_until(cancel.cancelled_owned())
        .boxed()
}

impl ModelPort for Routed {
    fn stream(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<ModelEvent, ModelError>> {
        let text = serde_json::to_string(&request.messages).unwrap_or_default();
        match &request.source {
            QuerySource::MainThread => self.main.stream(request, cancel),
            QuerySource::Subagent(_) if text.contains(MARK_A) => {
                let index = self.a.requests().len();
                let scripted = self.a.stream(request, cancel.clone());
                if self.a_stall_at == Some(index) {
                    return stalled(cancel);
                }
                scripted
            }
            QuerySource::Subagent(_) if text.contains(MARK_B) => self.b.stream(request, cancel),
            _ => {
                let _ = self.side.stream(request, cancel);
                stream::iter(text_response(&["side"])).boxed()
            }
        }
    }
}

struct Harness {
    _dirs: Vec<tempfile::TempDir>,
    root: PathBuf,
    kernel: LocalKernel,
    model: Arc<Routed>,
    store: Arc<NodeStore>,
}

/// 先定工作区，再按它的根写剧本（剧本里的路径是绝对路径）。
fn harness(workspace_parent: Option<&Path>, a_stall_at: Option<usize>) -> Harness {
    let system = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(system.path()).unwrap();
    let home = base.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let mut dirs = vec![system];
    let root = match workspace_parent {
        Some(parent) => {
            std::fs::create_dir_all(parent).unwrap();
            let dir = tempfile::tempdir_in(parent).unwrap();
            let root = std::fs::canonicalize(dir.path()).unwrap();
            dirs.push(dir);
            root
        }
        None => {
            let root = base.join("work");
            std::fs::create_dir_all(&root).unwrap();
            root
        }
    };
    let model = routed(&root, a_stall_at);
    let paths = StoragePaths::resolve(&StorageSettings::default(), &home, &home);
    let clock = Arc::new(SystemClock::utc());
    let ids = Arc::new(NodeIds::new(clock.clone()));
    let store = Arc::new(NodeStore::new(paths, ids.clone()));
    // 与入口相同：输出文件放在系统临时目录（cli `shell::config`），这里换成临时 HOME 旁边的目录。
    let shell = ShellConfig {
        scratch: base.join("zcode-shell/sessions"),
        ledger: base.join("zcode-shell/ledger"),
        bash: Some(PathBuf::from("/bin/bash")),
        bwrap: None,
        embedded: None,
        env: vec![
            (
                "PATH".to_owned(),
                "/usr/bin:/bin:/usr/sbin:/sbin".to_owned(),
            ),
            ("HOME".to_owned(), home.to_string_lossy().into_owned()),
        ]
        .into(),
    };
    let ports = Ports {
        model: model.clone(),
        environments: environments(Arc::new(
            LocalEnvironment::new(Some(home)).with_shell(shell),
        )),
        store: store.clone(),
        artifacts: store.clone(),
        clock,
        ids,
    };
    let kernel = LocalKernel::new(
        ports,
        Defaults {
            selection: selection(),
            mode: PermissionMode::Default,
            max_turns: None,
            search_tools: false,
            task_tools: None,
            // 前台子代理只在 fork 关着时有（`ZCODE_FORK_SUBAGENT=0`，修复线 L-A，agents.md §8.1.1）。
            limits: rust_cli_runtime::Limits {
                fork_subagent: Some(false),
                ..Default::default()
            },
        },
    );
    Harness {
        _dirs: dirs,
        root,
        kernel,
        model,
        store,
    }
}

fn routed(root: &Path, a_stall_at: Option<usize>) -> Arc<Routed> {
    let target = root.join("breakout/index.html");
    let notebook = root.join("breakout/notes.ipynb");
    let a = ScriptedModel::new(vec![
        one(
            "toolu_a1",
            "Bash",
            json!({"command": format!("mkdir -p {}/breakout && ls {}", root.display(), root.display()), "description": "Create the folder"}),
        ),
        one(
            "toolu_a2",
            "Write",
            json!({"file_path": target.to_string_lossy(), "content": "<!doctype html>\n<title>Breakout</title>\n"}),
        ),
        one(
            "toolu_a3",
            "Edit",
            json!({"file_path": target.to_string_lossy(), "old_string": "Breakout", "new_string": "Breakout game"}),
        ),
        one(
            "toolu_a4",
            "Write",
            json!({"file_path": notebook.to_string_lossy(), "content": NOTEBOOK}),
        ),
        one(
            "toolu_a5",
            "NotebookEdit",
            json!({"notebook_path": notebook.to_string_lossy(), "cell_id": "cell-0", "new_source": "print(2)"}),
        ),
        text_response(&["A done."]),
    ]);
    // 后台 B：写入都被用户拒绝（[`from_b`]），连写几次再结束。
    let mut b_script: Vec<Events> = (0..5)
        .map(|index| {
            one(
                &format!("toolu_b{index}"),
                "Write",
                json!({"file_path": root.join(format!("b{index}.txt")).to_string_lossy(), "content": "b"}),
            )
        })
        .collect();
    b_script.push(text_response(&["B done."]));
    Arc::new(Routed {
        main: ScriptedModel::new(vec![
            tools_response(&[
                (
                    "toolu_main_a",
                    "Agent",
                    json!({"description": "Build the page", "prompt": format!("{MARK_A}: create breakout/index.html"), "subagent_type": "general-purpose", "run_in_background": false}),
                ),
                (
                    "toolu_main_b",
                    "Agent",
                    json!({"description": "Write notes", "prompt": format!("{MARK_B}: write notes"), "subagent_type": "general-purpose", "run_in_background": true}),
                ),
            ]),
            text_response(&["Main done."]),
            // 后台 B 结束后的通知回合（交互模式）。
            text_response(&["Noted."]),
            text_response(&["Noted again."]),
        ]),
        a,
        b: ScriptedModel::new(b_script),
        side: ScriptedModel::default(),
        a_stall_at,
    })
}

fn result_texts(request: &ModelRequest) -> Vec<(String, bool)> {
    request
        .messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|content| match &content.block {
            ContentBlock::ToolResult {
                content, is_error, ..
            } => Some((
                content
                    .iter()
                    .filter_map(|part| match part {
                        ToolResultContent::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                *is_error,
            )),
            _ => None,
        })
        .collect()
}

/// 扮演前端：Bash 的询问答“允许并记住 `mkdir:*`”，其余询问答允许；等主线程第一个回合结束、后台 B 也结束。
///
/// 修 bug（F17）：以前只等主线程第一个回合结束就检查 B 的请求数。B 在后台跑，与主线程的回合没有先后保证：
/// 后台 B 的询问（它的调用是 `toolu_b*`）：测试扮演的用户一律拒绝。
fn from_b(request: &InteractionRequest) -> bool {
    request.tool_use().as_str().starts_with("toolu_b")
}

/// B 的询问拒绝；A 的 Bash 允许并记住 `mkdir:*`，A 的其余询问允许。
fn answer_for(request: &InteractionRequest, tool: &str) -> Value {
    if from_b(request) {
        json!({"decision": "deny"})
    } else if tool == "Bash" {
        json!({"decision": "allow", "remember": [{"tool": "Bash", "content": "mkdir:*"}]})
    } else {
        json!({"decision": "allow"})
    }
}

/// 负载下（外接盘上的工作区读规则文件要几秒）B 的第一个请求可能晚于 A 跑完、主回合结束，
/// 偶发 `B made 0 requests`（负载下全量连跑 24 次里 4 次）。现在等 B 的终态（`BackgroundWork`）再检查。
async fn drive(harness: &Harness) -> (Vec<String>, Arc<dyn SessionHandle>) {
    let workspace = WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(harness.root.to_string_lossy()),
    };
    let id = harness
        .kernel
        .create_session(CreateSession {
            id: None,
            workspace,
            selection: selection(),
            mode: PermissionMode::Default,
            plan: false,
        })
        .await
        .unwrap();
    let session = harness.kernel.open_session(&id).await.unwrap();
    let mut events = session.subscribe(Some(0));
    session
        .send(CommandEnvelope {
            id: CommandId::new("c1"),
            command: Command::Submit {
                text: "Build the breakout page with two agents.".to_owned(),
                attachments: Vec::new(),
                delivery: Delivery::Queue,
            },
        })
        .await;
    let mut seen = Vec::new();
    let mut answered = 0;
    let (mut main_finished, mut background_finished) = (false, false);
    loop {
        let next = tokio::time::timeout(Duration::from_secs(20), events.next()).await;
        let Ok(Some(event)) = next else {
            seen.push(format!(
                "TIMEOUT: A requests={}, B requests={}, main requests={}",
                harness.model.a.requests().len(),
                harness.model.b.requests().len(),
                harness.model.main.requests().len()
            ));
            return (seen, session);
        };
        match event.event {
            KernelEvent::InteractionAsked {
                interaction,
                request,
            } => {
                let InteractionRequest::Permission { tool, .. } = &request else {
                    panic!("unexpected interaction {request:?}");
                };
                seen.push(format!("asked {tool}"));
                let answer = answer_for(&request, tool);
                answered += 1;
                let ack = session
                    .send(CommandEnvelope {
                        id: CommandId::new(format!("answer-{answered}")),
                        command: Command::ResolveInteraction {
                            interaction,
                            answer,
                        },
                    })
                    .await;
                seen.push(format!("resolved {tool}: {ack:?}"));
            }
            KernelEvent::TurnFinished { outcome, .. } => {
                seen.push(format!("turn finished {outcome:?}"));
                main_finished = true;
            }
            // 剧本里只有 B 在后台（A 的 Bash 在前台）：后台工作的终态就是 B 的。
            KernelEvent::BackgroundWork { work, status } => {
                seen.push(format!("background {work} {status}"));
                background_finished = true;
            }
            _ => {}
        }
        if main_finished && background_finished {
            return (seen, session);
        }
    }
}

async fn check(harness: Harness) {
    let (seen, session) = drive(&harness).await;
    let a = harness.model.a.requests();
    let b = harness.model.b.requests();
    eprintln!("events: {seen:#?}");
    for (index, request) in a.iter().enumerate().skip(1) {
        eprintln!(
            "A request {index} results: {:?}",
            result_texts(request).last()
        );
    }
    for (index, request) in b.iter().enumerate().skip(1) {
        eprintln!(
            "B request {index} results: {:?}",
            result_texts(request).last()
        );
    }
    assert!(
        !seen.iter().any(|line| line.starts_with("TIMEOUT")),
        "the foreground agent stopped: {seen:#?}"
    );
    // A：起始请求、Bash、Write、Edit、Write（notebook）、NotebookEdit 之后，共 6 个请求。
    assert_eq!(a.len(), 6, "{seen:#?}");
    let after_bash = result_texts(&a[1]);
    assert!(!after_bash.last().unwrap().1, "{after_bash:?}");
    let after_write = result_texts(&a[2]);
    let (text, is_error) = after_write.last().unwrap();
    assert!(!is_error, "Write failed: {text}");
    assert!(text.starts_with("File created successfully at: "), "{text}");
    let after_edit = result_texts(&a[3]);
    let (text, is_error) = after_edit.last().unwrap();
    assert!(!is_error, "Edit failed: {text}");
    assert_eq!(
        std::fs::read_to_string(harness.root.join("breakout/index.html")).unwrap(),
        "<!doctype html>\n<title>Breakout game</title>\n"
    );
    let after_notebook = result_texts(&a[5]);
    let (text, is_error) = after_notebook.last().unwrap();
    assert!(!is_error, "NotebookEdit failed: {text}");
    let notebook = std::fs::read_to_string(harness.root.join("breakout/notes.ipynb")).unwrap();
    assert!(notebook.contains("print(2)"), "{notebook}");
    // 每次写入都在子会话记了 checkpoint（artifact 在系统盘的临时 HOME 下，与工作区不同卷时也成功）。
    let agents = session.subagents().await.unwrap();
    let child = agents.iter().find(|entry| !entry.background).unwrap();
    let loaded = harness.store.load(&child.child).await.unwrap();
    assert_eq!(loaded.checkpoints.len(), 4, "{:?}", loaded.checkpoints);
    // B 的写入都被拒绝（用户拒绝），但它照样跑完。
    assert!(b.len() >= 2, "B made {} requests", b.len());
    assert!(result_texts(&b[1]).last().unwrap().1);
    // 主线程收到 A 的报告。
    let main = harness.model.main.requests();
    assert!(main.len() >= 2, "{seen:#?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_foreground_agent_continues_after_an_approved_and_remembered_bash() {
    check(harness(None, None)).await;
}

/// 工作区在与系统临时目录不同的卷上（本机：外接盘上的 target 目录）。同一个卷时照样跑，只是覆盖不到跨卷。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn writes_on_another_volume_than_the_temp_dir_succeed() {
    let harness = harness(Some(Path::new(env!("CARGO_TARGET_TMPDIR"))), None);
    let device =
        |path: &Path| std::os::unix::fs::MetadataExt::dev(&std::fs::metadata(path).unwrap());
    eprintln!(
        "workspace {} on device {}, temp dir on device {}",
        harness.root.display(),
        device(&harness.root),
        device(&std::env::temp_dir())
    );
    check(harness).await;
}

fn text_of(message: &Message) -> String {
    match message {
        Message::User(user) => serde_json::to_string(&user.content).unwrap(),
        Message::Assistant(assistant) => format!(
            "assistant {}",
            serde_json::to_string(&assistant.content).unwrap()
        ),
        Message::Attachment(_) => "attachment".to_owned(),
        Message::System(_) => "system".to_owned(),
    }
}

/// 事故的记录形状：A 的 Bash 批准并记住、执行完、结果落库之后，A 的下一次模型请求开着但一直没有完成的块
/// （服务端还在推理、在重试，或者流停住了）；用户中断。A 的子会话以 `[Request interrupted by user]` 结束，
/// 中间没有 assistant 消息、没有新的操作意图与待回答的交互——与用户 db 里的记录相同。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stalled_request_after_the_approval_leaves_the_incident_trail() {
    let harness = harness(None, Some(1));
    let workspace = WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(harness.root.to_string_lossy()),
    };
    let id = harness
        .kernel
        .create_session(CreateSession {
            id: None,
            workspace,
            selection: selection(),
            mode: PermissionMode::Default,
            plan: false,
        })
        .await
        .unwrap();
    let session = harness.kernel.open_session(&id).await.unwrap();
    let mut events = session.subscribe(Some(0));
    session
        .send(CommandEnvelope {
            id: CommandId::new("c1"),
            command: Command::Submit {
                text: "Build the breakout page with two agents.".to_owned(),
                attachments: Vec::new(),
                delivery: Delivery::Queue,
            },
        })
        .await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut interrupted = false;
    loop {
        // A 的第二个请求已经发出、流开着：扮演用户等了一会儿后中断（只用于测试判定的轮询）。
        if !interrupted && harness.model.a.requests().len() >= 2 {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let ack = session
                .send(CommandEnvelope {
                    id: CommandId::new("interrupt"),
                    command: Command::Interrupt,
                })
                .await;
            eprintln!("interrupt: {ack:?}");
            interrupted = true;
        }
        let next = tokio::time::timeout_at(
            deadline.min(tokio::time::Instant::now() + Duration::from_millis(100)),
            events.next(),
        )
        .await;
        assert!(
            tokio::time::Instant::now() < deadline,
            "the run did not finish"
        );
        let Ok(Some(event)) = next else { continue };
        match event.event {
            KernelEvent::InteractionAsked {
                interaction,
                request,
            } => {
                let InteractionRequest::Permission { tool, .. } = &request else {
                    panic!("unexpected interaction {request:?}");
                };
                // A 只问 Bash；B 的写入也会问（被拒绝）。
                assert!(from_b(&request) || tool == "Bash", "{request:?}");
                let answer = answer_for(&request, tool);
                session
                    .send(CommandEnvelope {
                        id: CommandId::new(format!("answer-{}", request.tool_use())),
                        command: Command::ResolveInteraction {
                            interaction,
                            answer,
                        },
                    })
                    .await;
            }
            KernelEvent::TurnFinished { outcome, .. } => {
                eprintln!("turn finished {outcome:?}");
                break;
            }
            _ => {}
        }
    }
    assert!(interrupted);
    let agents = session.subagents().await.unwrap();
    let a = agents.iter().find(|entry| !entry.background).unwrap();
    let loaded = harness.store.load(&a.child).await.unwrap();
    let trail: Vec<String> = loaded.messages.iter().map(text_of).collect();
    eprintln!("A's session: {trail:#?}");
    let last = trail.last().unwrap();
    assert!(
        last.contains("[Request interrupted by user]") && !last.contains("for tool use"),
        "{last}"
    );
    let bash = trail
        .iter()
        .rposition(|text| text.contains("toolu_a1") && text.contains("tool_result"))
        .unwrap();
    assert!(
        !trail[bash + 1..]
            .iter()
            .any(|text| text.starts_with("assistant")),
        "no assistant message after the Bash result: {trail:#?}"
    );
    assert!(
        loaded.open_operations.is_empty(),
        "{:?}",
        loaded.open_operations
    );
    assert!(loaded.pending_interactions.is_empty());
    let parent = harness.store.load(&id).await.unwrap();
    let handback = parent
        .messages
        .iter()
        .map(text_of)
        .find(|text| text.contains("toolu_main_a") && text.contains("tool_result"))
        .unwrap();
    eprintln!("parent hand-back: {handback}");
    assert!(handback.contains("\"is_error\":true"), "{handback}");
}
