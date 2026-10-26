//! 事件日志的补发与在线时折叠出的画面相同（docs/specs/rust-cli/session-lifecycle.md §2.1，审查 R1-4）。
//!
//! 真实的内核组装（LocalKernel、env-local、node-store）跑一个长回合：推理与正文各几百个流增量、Bash 的输出进度、
//! 前台子代理（它自己的流增量与 Bash 进度经 Agent 工具的进度到父会话）。一直在线的订阅方收到全部事件——就是以前
//! 日志原样补发的内容。两个时点比较：第一个响应停在正文块中间时（流式的行还没有定稿），以及回合结束后。每个时点
//! 从 0 重新订阅、以及从中途各处重新订阅（订阅方落后后按最后折叠的 seq 续上），按 TUI 的 `view::Transcript`
//! 折叠，每一行与状态行的字数都与在线时相同，而补发的事件少得多。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "集成测试：准备临时目录与真实的执行环境"
)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::stream::{self, BoxStream};
use futures_util::{FutureExt, StreamExt};
use rust_cli_env_local::{LocalEnvironment, ShellConfig};
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{CommandId, EnvironmentId, ToolUseId};
use rust_cli_kernel_api::message::{ContentBlock, StopReason, Usage};
use rust_cli_kernel_api::model::{
    BlockDelta, ModelError, ModelEvent, ModelPort, ModelRequest, QuerySource,
};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{
    Ack, Command, CommandEnvelope, CreateSession, Delivery, Kernel, KernelEvent, SequencedEvent,
};
use rust_cli_kernel_api::testing::{ScriptedModel, environments, selection, text_response};
use rust_cli_node_store::{NodeIds, NodeStore, SystemClock};
use rust_cli_paths::{StoragePaths, StorageSettings};
use rust_cli_runtime::{Defaults, LocalKernel, Ports};
use rust_cli_tui::view::{Body, Transcript};
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

const MARK: &str = "F8-REPLAY-CHILD";
/// 折叠时用同一个时刻：推理用时等按时间算的字段两边相同。
const NOW: u64 = 1_792_314_307_000;

type Events = Vec<Result<ModelEvent, ModelError>>;

enum Part {
    Thinking(usize),
    Text(usize),
    Tool(&'static str, &'static str, Value),
}

/// 一次响应：每个推理 / 正文块拆成 `n` 个增量，推理块带签名；有工具时以 tool_use 结束。
fn response(parts: Vec<Part>) -> Events {
    let mut events = vec![Ok(ModelEvent::MessageStart {
        id: "msg_api".to_owned(),
        model: "scripted".to_owned(),
        usage: Usage::default(),
    })];
    let mut tool = false;
    for (index, part) in parts.into_iter().enumerate() {
        match part {
            Part::Thinking(n) => {
                events.push(Ok(ModelEvent::BlockStart {
                    index,
                    block: ContentBlock::Thinking {
                        thinking: String::new(),
                        signature: None,
                    },
                }));
                events.extend((0..n).map(|i| {
                    Ok(ModelEvent::BlockDelta {
                        index,
                        delta: BlockDelta::Thinking(format!("t{i} ")),
                    })
                }));
                events.push(Ok(ModelEvent::BlockDelta {
                    index,
                    delta: BlockDelta::Signature("sig".to_owned()),
                }));
            }
            Part::Text(n) => {
                events.push(Ok(ModelEvent::BlockStart {
                    index,
                    block: ContentBlock::Text {
                        text: String::new(),
                    },
                }));
                events.extend((0..n).map(|i| {
                    Ok(ModelEvent::BlockDelta {
                        index,
                        delta: BlockDelta::Text(format!("w{i} ")),
                    })
                }));
            }
            Part::Tool(id, name, input) => {
                tool = true;
                events.extend([
                    Ok(ModelEvent::BlockStart {
                        index,
                        block: ContentBlock::ToolUse {
                            id: ToolUseId::new(id),
                            name: name.to_owned(),
                            input: json!({}),
                        },
                    }),
                    Ok(ModelEvent::BlockDelta {
                        index,
                        delta: BlockDelta::InputJson(input.to_string()),
                    }),
                ]);
            }
        }
        events.push(Ok(ModelEvent::BlockStop { index }));
    }
    let stop = if tool {
        StopReason::ToolUse
    } else {
        StopReason::EndTurn
    };
    events.extend([
        Ok(ModelEvent::MessageDelta {
            stop_reason: Some(stop),
            usage: Usage::default(),
        }),
        Ok(ModelEvent::MessageStop),
    ]);
    events
}

fn bash(lines: usize) -> Value {
    json!({
        "command": format!("for i in $(seq 1 {lines}); do echo line-$i; sleep 0.01; done"),
        "description": "Print lines",
    })
}

/// 第一个主请求停在这么多个事件之后（推理块全部、正文块的一半），等测试放行。
const PAUSE_AFTER: usize = 1 + 1 + 120 + 1 + 1 + 1 + 100;
const PAUSED_DELTAS: usize = 120 + 1 + 100;

/// 主线程按剧本；子代理按 prompt 里的标记；其余（标题等旁路请求）回一段文字。
struct Routed {
    main: ScriptedModel,
    child: ScriptedModel,
    /// 第一个主请求的放行信号。
    release: Mutex<Option<oneshot::Receiver<()>>>,
}

impl ModelPort for Routed {
    fn stream(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<ModelEvent, ModelError>> {
        let text = serde_json::to_string(&request.messages).unwrap_or_default();
        match &request.source {
            QuerySource::MainThread => {
                let mut scripted = self.main.stream(request, cancel);
                let Some(release) = self.release.lock().unwrap().take() else {
                    return scripted;
                };
                // 剧本的流是现成的：取出来分成两半，中间等放行。
                let mut events = Vec::new();
                while let Some(Some(event)) = scripted.next().now_or_never() {
                    events.push(event);
                }
                let tail = events.split_off(PAUSE_AFTER);
                let rest = stream::once(release).flat_map(move |_| stream::iter(tail.clone()));
                stream::iter(events).chain(rest).boxed()
            }
            QuerySource::Subagent(_) if text.contains(MARK) => self.child.stream(request, cancel),
            _ => stream::iter(text_response(&["side"])).boxed(),
        }
    }
}

fn kernel(home: PathBuf, scratch: PathBuf, release: oneshot::Receiver<()>) -> LocalKernel {
    let model = Arc::new(Routed {
        release: Mutex::new(Some(release)),
        main: ScriptedModel::new(vec![
            response(vec![
                Part::Thinking(120),
                Part::Text(200),
                Part::Tool("toolu_bash", "Bash", bash(30)),
            ]),
            response(vec![
                Part::Text(100),
                Part::Tool(
                    "toolu_agent",
                    "Agent",
                    json!({"description": "Look around", "prompt": format!("{MARK}: look"), "subagent_type": "general-purpose", "run_in_background": false}),
                ),
            ]),
            response(vec![Part::Thinking(40), Part::Text(150)]),
        ]),
        child: ScriptedModel::new(vec![
            response(vec![
                Part::Text(60),
                Part::Tool("toolu_child_bash", "Bash", bash(20)),
            ]),
            response(vec![Part::Thinking(40), Part::Text(50)]),
        ]),
    });
    let paths = StoragePaths::resolve(&StorageSettings::default(), &home, &home);
    let clock = Arc::new(SystemClock::utc());
    let ids = Arc::new(NodeIds::new(clock.clone()));
    let store = Arc::new(NodeStore::new(paths, ids.clone()));
    let shell = ShellConfig {
        scratch: scratch.join("sessions"),
        ledger: scratch.join("ledger"),
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
        model,
        environments: environments(Arc::new(
            LocalEnvironment::new(Some(home)).with_shell(shell),
        )),
        store: store.clone(),
        artifacts: store,
        clock,
        ids,
    };
    LocalKernel::new(
        ports,
        Defaults {
            selection: selection(),
            mode: PermissionMode::BypassPermissions,
            max_turns: None,
            search_tools: false,
            task_tools: None,
            // 前台子代理只在 fork 关着时有（`ZCODE_FORK_SUBAGENT=0`，修复线 L-A，agents.md §8.1.1）。
            limits: rust_cli_runtime::Limits {
                fork_subagent: Some(false),
                ..Default::default()
            },
        },
    )
}

/// 已经在日志里、`seq ≤ last` 的补发。
fn drain(stream: &mut BoxStream<'static, SequencedEvent>, last: u64) -> Vec<SequencedEvent> {
    let mut events = Vec::new();
    while let Some(Some(event)) = stream.next().now_or_never() {
        if event.seq <= last {
            events.push(event);
        }
    }
    events
}

fn fold(transcript: &mut Transcript, events: &[SequencedEvent]) {
    for event in events {
        transcript.apply(event.clone(), NOW);
    }
}

fn bodies(transcript: &Transcript) -> Vec<Body> {
    transcript
        .rows()
        .iter()
        .map(|row| row.body.clone())
        .collect()
}

/// 一个时点的比较：在线订阅方到 `last` 为止折叠出的画面，与从 0 补发、从中途各处续上的画面逐行相同；返回补发的
/// 事件数。
fn compare(
    handle: &Arc<dyn rust_cli_kernel_api::session_api::SessionHandle>,
    workspace: &WorkspaceRef,
    online: &[SequencedEvent],
    last: u64,
) -> usize {
    let fresh = || {
        Transcript::new(
            selection(),
            PermissionMode::BypassPermissions,
            workspace.clone(),
        )
    };
    let mut expected = fresh();
    fold(&mut expected, online);
    let rows = bodies(&expected);
    let replayed = drain(&mut handle.subscribe(Some(0)), last);
    let mut late = fresh();
    fold(&mut late, &replayed);
    assert_eq!(bodies(&late), rows);
    assert_eq!(late.streamed_chars(), expected.streamed_chars());
    assert_eq!(late.title(), expected.title());
    for cut in (1..online.len()).step_by(7) {
        let mut resumed = fresh();
        fold(&mut resumed, &online[..cut]);
        fold(
            &mut resumed,
            &drain(&mut handle.subscribe(Some(online[cut - 1].seq)), last),
        );
        assert_eq!(bodies(&resumed), rows, "resumed after {cut} events");
        assert_eq!(resumed.streamed_chars(), expected.streamed_chars());
    }
    replayed.len()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_late_subscriber_folds_the_same_screen_from_fewer_events() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (home, root) = (base.join("home"), base.join("work"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&root).unwrap();
    let (release, paused) = oneshot::channel();
    let kernel = kernel(home, base.join("zcode-shell"), paused);
    let workspace = WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(root.to_string_lossy()),
    };
    let id = kernel
        .create_session(CreateSession {
            id: None,
            workspace: workspace.clone(),
            selection: selection(),
            mode: PermissionMode::BypassPermissions,
            plan: false,
        })
        .await
        .unwrap();
    let handle = kernel.open_session(&id).await.unwrap();
    let mut live = handle.subscribe(Some(0));
    let ack = handle
        .send(CommandEnvelope {
            id: CommandId::new("cmd_1"),
            command: Command::Submit {
                text: "go".to_owned(),
                attachments: Vec::new(),
                delivery: Delivery::Queue,
            },
        })
        .await;
    assert!(!matches!(ack, Ack::Rejected { .. }), "{ack:?}");
    let mut online = Vec::new();
    // 时点一：第一个响应停在正文块中间。
    tokio::time::timeout(Duration::from_secs(60), async {
        let mut deltas = 0;
        while deltas < PAUSED_DELTAS {
            let event = live.next().await.expect("the stream goes on");
            deltas += usize::from(matches!(event.event, KernelEvent::StreamDelta { .. }));
            online.push(event);
        }
    })
    .await
    .expect("the first response streams up to the pause");
    let last = handle.view().await.unwrap().last_seq;
    online.extend(drain(&mut live, last));
    online.retain(|event| event.seq <= last);
    let paused_replay = compare(&handle, &workspace, &online, last);
    assert!(
        paused_replay * 10 < online.len(),
        "{paused_replay} of {}",
        online.len()
    );

    // 时点二：回合结束。
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(60), async {
        while let Some(event) = live.next().await {
            let finished = matches!(event.event, KernelEvent::TurnFinished { .. });
            online.push(event);
            if finished {
                break;
            }
        }
    })
    .await
    .expect("the turn finishes");
    let last = handle.view().await.unwrap().last_seq;
    online.extend(drain(&mut live, last));
    online.retain(|event| event.seq <= last);
    assert!(
        online.iter().any(|event| matches!(
            &event.event,
            KernelEvent::ToolProgress { progress, .. } if progress.data.get("agentId").is_some()
        )),
        "the subagent reports progress to the parent"
    );
    let replayed = compare(&handle, &workspace, &online, last);
    assert!(
        replayed * 3 < online.len(),
        "{replayed} of {}",
        online.len()
    );
}
