//! 子代理快照（tui-panels.md §1）：时刻、等待、版本、变化通知与只读查看。

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures_util::StreamExt;
use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::clock::Clock;
use rust_cli_kernel_api::ids::{AgentId, SessionId};
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::panels::{SubagentStatus, TasksSnapshot};
use serde_json::json;
use tokio::sync::Notify;

use super::*;
use crate::live::{AgentState, Live};
use crate::report::Ending;
use crate::test_support::{harness, text};

/// 测试里手动拨动的时钟。
#[derive(Default)]
struct Dial(AtomicU64);

impl Clock for Dial {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }

    fn time_zone(&self) -> String {
        "UTC".to_owned()
    }
}

fn running(agent: &str, root: &str) -> crate::live::LiveAgent {
    crate::live::LiveAgent {
        machine: None,
        agent: AgentId::new(agent),
        child: SessionId::new(format!("sess_subagent_{agent}")),
        parent: SessionId::new(root),
        root: SessionId::new(root),
        owner: None,
        depth: 1,
        tool_use: None,
        description: format!("work of {agent}"),
        definition: Arc::new(
            crate::definitions::builtins()
                .into_iter()
                .find(|definition| definition.agent_type == "Explore")
                .unwrap(),
        ),
        workspace: crate::test_support::workspace(),
        selection: rust_cli_kernel_api::testing::selection(),
        mode: rust_cli_kernel_api::permission::PermissionMode::Default,
        background: true,
        asks: None,
        state: AgentState::Running {
            abort: Abort::new(),
            mailbox: Vec::new(),
        },
        shown: crate::live::Shown::default(),
        fork_prefix: None,
    }
}

#[test]
fn times_follow_the_state_and_restart_resets_the_clock() {
    let clock = Arc::new(Dial::default());
    let live = Live::new(clock.clone());
    clock.0.store(1_000, Ordering::SeqCst);
    live.insert(running("agent_a", "sess_root"));
    live.insert(running("agent_b", "sess_other"));
    clock.0.store(4_000, Ordering::SeqCst);
    let root = SessionId::new("sess_root");
    let listed = entries(&live, &root);
    assert_eq!(listed.len(), 1, "only the root's agents");
    assert_eq!(listed[0].status, SubagentStatus::Running);
    assert_eq!(listed[0].elapsed_ms, 3_000);
    assert_eq!(listed[0].ended_ago_ms, None);

    live.finish(&AgentId::new("agent_a"), Ending::Failed);
    clock.0.store(10_000, Ordering::SeqCst);
    let entry = &entries(&live, &root)[0];
    assert_eq!(entry.status, SubagentStatus::Failed);
    assert_eq!(entry.elapsed_ms, 3_000, "stops at the end of the run");
    assert_eq!(entry.ended_ago_ms, Some(6_000));

    // SendMessage 续跑：重新开始计时。
    assert!(live.restart(&AgentId::new("agent_a"), Abort::new()));
    clock.0.store(12_000, Ordering::SeqCst);
    let entry = &entries(&live, &root)[0];
    assert_eq!(entry.status, SubagentStatus::Running);
    assert_eq!(entry.elapsed_ms, 2_000);
    assert_eq!(entry.ended_ago_ms, None);
}

/// 修复线 F30（tui-panels.md §1.1）：停放的条目带 `parked`、状态仍是 completed，owner 原样给出；真的结束了的没有。
#[test]
fn parked_agents_are_marked_in_the_snapshot() {
    let clock = Arc::new(Dial::default());
    let live = Live::new(clock.clone());
    live.insert(running("agent_p", "sess_root"));
    let mut child = running("agent_c", "sess_root");
    child.owner = Some(AgentId::new("agent_p"));
    live.insert(child);
    live.with(|agents| {
        agents.get_mut(&AgentId::new("agent_p")).unwrap().state =
            AgentState::Parked(crate::report::Report::default());
    });
    clock.0.store(5_000, Ordering::SeqCst);
    let root = SessionId::new("sess_root");
    let listed = entries(&live, &root);
    assert_eq!(listed[0].status, SubagentStatus::Completed);
    assert!(listed[0].parked);
    assert_eq!(listed[0].ended_ago_ms, Some(5_000));
    assert!(!listed[1].parked);
    assert_eq!(listed[1].owner, Some(AgentId::new("agent_p")));
    live.finish(&AgentId::new("agent_p"), Ending::Completed);
    let listed = entries(&live, &root);
    assert_eq!(listed[0].status, SubagentStatus::Completed);
    assert!(!listed[0].parked);
}

/// 修复线 F34（tui-panels.md §0）：子代理条目与在跑的后台子代理出自 `live::Live` 的同一次读取。条目按启动顺序；后台子代理
/// 与 `background_entries` 同一个筛选与顺序（按 ID）：在跑与停放的后台子代理在两边，前台的与结束了的只在条目里，别的
/// 顶层会话的都不在。
#[test]
fn the_tasks_snapshot_reads_the_rows_and_the_background_agents_together() {
    let live = Live::default();
    live.insert(running("agent_b", "sess_root"));
    live.insert(running("agent_a", "sess_root"));
    let mut foreground = running("agent_f", "sess_root");
    foreground.background = false;
    live.insert(foreground);
    live.insert(running("agent_x", "sess_other"));
    live.with(|agents| {
        agents.get_mut(&AgentId::new("agent_a")).unwrap().state =
            AgentState::Parked(crate::report::Report::default());
    });
    let root = SessionId::new("sess_root");
    let ids = |snapshot: &TasksSnapshot| {
        (
            snapshot
                .subagents
                .iter()
                .map(|entry| entry.agent.to_string())
                .collect::<Vec<_>>(),
            snapshot
                .background
                .iter()
                .map(|entry| entry.id.clone())
                .collect::<Vec<_>>(),
        )
    };
    let snapshot = tasks(&live, &root);
    assert_eq!(
        ids(&snapshot),
        (
            vec!["agent_b".into(), "agent_a".into(), "agent_f".into()],
            vec!["agent_a".into(), "agent_b".into()]
        )
    );
    assert_eq!(snapshot.subagents, entries(&live, &root));
    let background: Vec<_> = live
        .background_in(&root)
        .iter()
        .map(background_entry)
        .collect();
    assert_eq!(snapshot.background, background);
    live.finish(&AgentId::new("agent_b"), Ending::Completed);
    let snapshot = tasks(&live, &root);
    assert_eq!(snapshot.subagents.len(), 3);
    assert_eq!(snapshot.subagents[0].status, SubagentStatus::Completed);
    assert_eq!(ids(&snapshot).1, vec!["agent_a".to_owned()]);
}

/// 远端执行 R4（remote-exec.md §21.6）：子代理条目与后台子代理带它的缺省机器（Agent 的 `machine`），本机的没有。
#[test]
fn entries_carry_the_default_machine() {
    let live = Live::default();
    let mut remote = running("agent_r", "sess_root");
    remote.machine = Some("dev-a".to_owned());
    live.insert(remote);
    live.insert(running("agent_l", "sess_root"));
    let root = SessionId::new("sess_root");
    let snapshot = tasks(&live, &root);
    let rows: Vec<_> = snapshot
        .subagents
        .iter()
        .map(|entry| (entry.agent.to_string(), entry.machine.clone()))
        .collect();
    assert_eq!(
        rows,
        [
            ("agent_r".to_owned(), Some("dev-a".to_owned())),
            ("agent_l".to_owned(), None)
        ]
    );
    let background: Vec<_> = snapshot
        .background
        .iter()
        .map(|entry| (entry.id.clone(), entry.machine.clone()))
        .collect();
    assert_eq!(
        background,
        [
            ("agent_l".to_owned(), None),
            ("agent_r".to_owned(), Some("dev-a".to_owned()))
        ]
    );
}

#[tokio::test]
async fn every_change_wakes_the_subscribers_and_versions_count_commits() {
    let live = Live::default();
    let mut changes = changes(&live);
    live.insert(running("agent_a", "sess_root"));
    changes.next().await.unwrap();
    live.waiting(&AgentId::new("agent_a"), true);
    changes.next().await.unwrap();
    let root = SessionId::new("sess_root");
    assert!(entries(&live, &root)[0].waiting);
    live.waiting(&AgentId::new("agent_a"), false);
    live.committed(&AgentId::new("agent_a"), None);
    live.committed(&AgentId::new("agent_a"), None);
    // 多次改动合并成一次通知；查询看到的是最后的状态。
    changes.next().await.unwrap();
    let entry = &entries(&live, &root)[0];
    assert!(!entry.waiting);
    assert_eq!(entry.version, 2);
}

#[tokio::test]
async fn a_background_agent_is_listed_and_viewed_read_only() {
    let gate = Arc::new(Notify::new());
    let harness = harness(Some(gate.clone()));
    let parent = SessionId::new("sess_parent");
    let signals = harness.agents.signals();
    let mut changes = signals.subagent_changes();
    let output = harness
        .call(
            "sess_parent",
            "Agent",
            json!({"description": "Find files", "prompt": "List the files.", "subagent_type": "Explore"}),
        )
        .await;
    assert!(text(&output).starts_with("Async agent launched successfully."));
    let listed = signals.subagents(&parent).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].agent_type, "Explore");
    assert_eq!(listed[0].description, "Find files");
    assert!(listed[0].background);
    assert_eq!(listed[0].status, SubagentStatus::Running);
    // 只读快照：子会话里的第一条消息是 prompt；在跑。
    let agent = listed[0].agent.clone();
    let view = signals.subagent_view(&parent, &agent).await.unwrap();
    assert!(view.running);
    assert!(matches!(&view.messages[..], [Message::User(_)]));
    // 别的顶层会话看不到它。
    let other = SessionId::new("sess_other");
    assert!(signals.subagents(&other).await.is_empty());
    assert!(signals.subagent_view(&other, &agent).await.is_err());

    changes.next().await.unwrap();
    gate.notify_one();
    // 子循环提交报告、结束：版本加一，状态是 completed。
    let finished = loop {
        let entries = signals.subagents(&parent).await;
        if entries[0].status != SubagentStatus::Running {
            break entries;
        }
        changes.next().await.unwrap();
    };
    assert_eq!(finished[0].status, SubagentStatus::Completed);
    assert_eq!(finished[0].version, 1);
    let view = signals.subagent_view(&parent, &agent).await.unwrap();
    assert!(!view.running);
    assert_eq!(view.messages.len(), 2);
}

/// L35（stall-visibility.md §4.3）：子循环事件折成当前活动；只在变化时通知、不加版本；只在跑的带活动。
/// 修复线 F26、F62：推理与工具调用都不是活动。
#[test]
fn the_current_activity_follows_the_child_events() {
    use rust_cli_kernel_api::ids::ToolUseId;
    use rust_cli_kernel_api::model::{BlockDelta, ModelError, ModelNotice};
    use rust_cli_kernel_api::orchestration::QueryEvent;
    use rust_cli_kernel_api::panels::SubagentActivity;
    use rust_cli_kernel_api::tool::ToolOutput;

    use crate::doing::Doing;

    let clock = Arc::new(Dial::default());
    let live = Live::new(clock.clone());
    live.insert(running("agent_a", "sess_root"));
    let agent = AgentId::new("agent_a");
    let root = SessionId::new("sess_root");
    let mut changes = live.subscribe();
    changes.borrow_and_update();
    let apply = |event: QueryEvent| {
        if let Some(doing) = Doing::after(&event) {
            live.doing(&agent, doing);
        }
    };
    let activity = || entries(&live, &root)[0].activity.clone();
    let thinking = |text: &str| QueryEvent::Delta {
        message: 0,
        index: 0,
        delta: BlockDelta::Thinking(text.to_owned()),
    };

    // 修复线 F26（用户决定）：推理增量不是活动（原来是 `Thinking`，列表行尾在它与工具名之间来回切换）。
    apply(thinking("Let me"));
    assert_eq!(activity(), None);
    assert!(!changes.has_changed().unwrap(), "no activity, no change");

    // 修复线 F62（用户决定）：工具调用也不是活动（原来是 `Tool{name}`，Explore 的 Bash 只跑几十毫秒，列表行的
    // ` · Bash` 一闪而过）：没有活动、不发变化通知、不加版本。
    let bash = || QueryEvent::ToolStarted {
        tool_use: ToolUseId::new("toolu_1"),
        name: "Bash".to_owned(),
        input: json!({"command": "ls"}),
    };
    apply(bash());
    assert_eq!(activity(), None);
    assert!(
        !changes.has_changed().unwrap(),
        "a tool call is no activity"
    );
    assert_eq!(entries(&live, &root)[0].version, 0);
    apply(QueryEvent::ToolFinished {
        tool_use: ToolUseId::new("toolu_1"),
        output: ToolOutput {
            content: Vec::new(),
            is_error: false,
            display: serde_json::Value::Null,
        },
    });
    assert_eq!(activity(), None);
    assert!(!changes.has_changed().unwrap());

    // 重试：按查询时刻算还要等多久。
    clock.0.store(1_000, Ordering::SeqCst);
    apply(QueryEvent::ModelNotice {
        notice: ModelNotice::Retrying {
            attempt: 2,
            max_retries: 10,
            delay_ms: 5_000,
            error: ModelError::Server {
                status: 500,
                message: "boom".to_owned(),
            },
        },
    });
    clock.0.store(3_000, Ordering::SeqCst);
    assert_eq!(
        activity(),
        Some(SubagentActivity::Retrying {
            attempt: 2,
            max_retries: 10,
            retry_in_ms: 3_000,
            error: "server error 500: boom".to_owned(),
        })
    );
    apply(QueryEvent::ModelNotice {
        notice: ModelNotice::Stalled {
            retry_in_ms: 280_000,
        },
    });
    clock.0.store(13_000, Ordering::SeqCst);
    assert_eq!(
        activity(),
        Some(SubagentActivity::Stalled {
            retry_in_ms: 270_000
        })
    );
    apply(QueryEvent::ModelNotice {
        notice: ModelNotice::Resumed,
    });
    assert_eq!(activity(), None);
    // 重试之后模型回应了一个工具调用：与文字增量相同，清掉重试、发一次变化通知（F62）。
    apply(QueryEvent::ModelNotice {
        notice: ModelNotice::Stalled { retry_in_ms: 1_000 },
    });
    changes.borrow_and_update();
    apply(bash());
    assert_eq!(activity(), None);
    assert!(changes.has_changed().unwrap());

    // 结束运行时清掉；续跑后没有旧的活动。
    apply(thinking("again"));
    live.finish(&agent, Ending::Completed);
    assert_eq!(activity(), None);
    assert!(live.restart(&agent, Abort::new()));
    assert_eq!(activity(), None);
}

/// 列表的快照只取这个顶层会话的条目，定义（含 system prompt 正文）共享、不复制（修复线 F6，审查 R5-11）。
#[test]
fn a_snapshot_takes_only_its_root_and_shares_definitions() {
    let live = Live::default();
    live.insert(running("agent_a", "sess_1"));
    live.insert(running("agent_b", "sess_2"));
    let (agents, _) = live.snapshot_in(&SessionId::new("sess_1"));
    assert_eq!(agents.len(), 1);
    let stored = live.get(&AgentId::new("agent_a")).unwrap();
    assert!(Arc::ptr_eq(&agents[0].definition, &stored.definition));
}

/// 修复线 F112（tui-panels.md §1.7）：子代理提交一个完成的子代理请求之后，`Live` 在加版本的同一次改动里记下它的输出速度
/// （查看时 footer 的 TPS）；失败的请求与子循环里的压缩请求不更新速度。修前 agents 不记速度，快照写死空。
#[tokio::test]
async fn a_completed_subagent_request_records_its_speed() {
    use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
    use rust_cli_kernel_api::ids::{EnvironmentId, MessageId, TraceId};
    use rust_cli_kernel_api::message::{AssistantMessage, ContentBlock, StopReason, Usage};
    use rust_cli_kernel_api::metering::{CallStatus, Metered, ModelCall, OutputSpeed};
    use rust_cli_kernel_api::model::QuerySource;
    use rust_cli_kernel_api::orchestration::RunSink;
    use rust_cli_kernel_api::store::{SessionKind, SessionMeta, SessionStore};
    use rust_cli_kernel_api::testing::{MemoryStore, SequentialIds, selection};

    let live = Arc::new(Live::default());
    let agent = AgentId::new("agent_a");
    live.insert(running("agent_a", "sess_root"));
    let store = Arc::new(MemoryStore::default());
    let child = SessionId::new("sess_subagent_agent_a");
    store
        .create(SessionMeta {
            id: child.clone(),
            kind: SessionKind::Subagent,
            parent: Some(SessionId::new("sess_root")),
            workspace: WorkspaceRef {
                environment: EnvironmentId::new("local"),
                root: EnvPath::new("/Users/dev/work"),
            },
            title: "work of agent_a".to_owned(),
            trace: TraceId::new("trace"),
            created_at_ms: 1,
        })
        .await
        .unwrap();
    let sink = crate::child::ChildSink::new(
        store,
        Arc::new(SequentialIds::default()),
        live.clone(),
        agent.clone(),
        child,
        MessageId::new("prompt_1"),
        None,
    );
    let reply = |id: &str| {
        Message::Assistant(AssistantMessage {
            id: MessageId::new(id),
            content: vec![ContentBlock::Text {
                text: "Found it.".to_owned(),
            }],
            model: "scripted".to_owned(),
            stop_reason: Some(StopReason::EndTurn),
            usage: None,
        })
    };
    let call = |id: &str, status: CallStatus, source: QuerySource, output: u64| ModelCall {
        message: MessageId::new(id),
        source,
        selection: selection(),
        started_at_ms: 1_000,
        first_token_at_ms: Some(2_000),
        first_content_at_ms: Some(2_000),
        completed_at_ms: 4_000,
        status,
        stop_reason: None,
        tool_calls: 0,
        usage: Some(Usage {
            input_tokens: 10,
            output_tokens: output,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        }),
        retries: 0,
        error: None,
    };
    let explore = || QuerySource::Subagent("Explore".to_owned());
    let shown = |live: &Live| {
        let found = live.get(&agent).unwrap();
        (found.shown.version, found.shown.speed)
    };
    // 完成的子代理请求：90 个输出、生成 2000 ms → 45 tok/s；版本加一。
    sink.commit_metered(
        Some(reply("msg_1")),
        Metered::Model(call("msg_1", CallStatus::Completed, explore(), 90)),
    )
    .await
    .unwrap();
    let first = OutputSpeed {
        output_tokens: Some(90),
        duration_ms: 3_000,
        first_content_ms: Some(1_000),
    };
    assert_eq!(shown(&live), (1, Some(first)));
    // 失败的请求：消息照样提交（版本加一），速度不变。
    sink.commit_metered(
        Some(reply("msg_2")),
        Metered::Model(call("msg_2", CallStatus::Error, explore(), 5)),
    )
    .await
    .unwrap();
    assert_eq!(shown(&live), (2, Some(first)));
    // 子循环里的压缩请求：不是子代理的主循环请求，速度不变。
    sink.commit_metered(
        Some(reply("msg_3")),
        Metered::Model(call(
            "msg_3",
            CallStatus::Completed,
            QuerySource::Compact,
            400,
        )),
    )
    .await
    .unwrap();
    assert_eq!(shown(&live), (3, Some(first)));
}

/// 修复线 F112：只读快照带子代理的 ctx 与上下文提示（runtime 注入的压缩器按子会话的历史与子代理的选择算），以及 `Live`
/// 记的最近一个请求的速度。修前三项都写死空。
#[tokio::test]
async fn the_view_carries_the_subagents_context_and_speed() {
    use rust_cli_kernel_api::compaction::{ContextNotice, ContextOccupancy};
    use rust_cli_kernel_api::metering::OutputSpeed;
    let gate = Arc::new(Notify::new());
    let harness = harness(Some(gate.clone()));
    let parent = SessionId::new("sess_parent");
    let signals = harness.agents.signals();
    let output = harness
        .call(
            "sess_parent",
            "Agent",
            json!({"description": "Find files", "prompt": "List the files.",
                   "subagent_type": "Explore", "effort": "high"}),
        )
        .await;
    assert!(text(&output).starts_with("Async agent launched successfully."));
    let agent = signals.subagents(&parent).await[0].agent.clone();
    let view = signals.subagent_view(&parent, &agent).await.unwrap();
    // 子代理自己的档位（Agent 的 `effort` 参数），父会话没有档位。
    assert_eq!(view.selection.reasoning.as_deref(), Some("high"));
    // 子会话里只有 prompt 一条：1000 / 10000；提示同一处算。
    assert_eq!(
        view.context_usage,
        Some(ContextOccupancy {
            used_tokens: 1_000,
            max_tokens: 10_000,
        })
    );
    assert_eq!(view.context_notice, ContextNotice::Used { percent: 1 });
    assert_eq!(
        harness.compactor.asked.lock().unwrap().last().cloned(),
        Some((view.selection.model.clone(), Some("high".to_owned())))
    );
    // 还没有完成的请求：没有速度。
    assert_eq!(view.output_speed, None);
    let speed = OutputSpeed {
        output_tokens: Some(90),
        duration_ms: 3_000,
        first_content_ms: Some(1_000),
    };
    harness.agents.core.live.committed(&agent, Some(speed));
    let view = signals.subagent_view(&parent, &agent).await.unwrap();
    assert_eq!(view.output_speed, Some(speed));
    gate.notify_one();
}
