//! 互操作的第十段：与 Node 共用数据的兼容（修复线 F2，docs/specs/rust-cli/node-store.md §6 第 10 步）。
//!
//! - `compat-write <storage-dir>`：写四个会话，输出会话 id：
//!   - 中断：完成的一轮（响应里的模型名带日期，请求的是目录 ID）与流式中被中断的一轮（没有停止原因）；
//!   - 提交前中断（F37）：输入、当步的 attachment、上下文宣布与中断标记，没有 assistant；
//!   - esc 撤回（F38）：完成的一轮之后同上，再按 `RestoreCancelled` 回退到这条输入之前（`session.revert`）；
//!   - 显示：bash 模式的输入与输出、skill 命令的标签与正文、`plan_mode_exit` 提醒；
//!   - 子代理：一次性子代理（Explore）的调用、子会话与结果；
//! - `compat-checkpoints <storage-dir> <session>`：在已有会话里追加一轮 Write，一个批次两个 checkpoint；
//! - `compat-scope <storage-dir> <session>`：在 Node 群协作会话（`permission.scope = session`）里按“总是允许”、
//!   用户改模式写入，输出本工程读到的会话规则与项目规则；
//! - `task-load <storage-dir> <session>` / `task-add <storage-dir> <session> <subject>`：任务清单的读与追加。
use rust_cli_kernel_api::agents::{TaskListStore, TaskRecord, TaskStatus};
use rust_cli_kernel_api::metering::{CallStatus, ModelCall};
use rust_cli_kernel_api::model::QuerySource;
use rust_cli_kernel_api::permission::{PermissionRule, RuleBehavior};
use rust_cli_kernel_api::store::{ArtifactPurpose, ArtifactStore, WorkspaceCheckpoint};

use super::*;

/// 响应里的模型名（网关常带日期）；请求用的是目录 ID `m`（Node 的 `modelId` 取后者）。
pub(super) const RESPONSE_MODEL: &str = "m-20260101";

fn called(message: &MessageId, status: CallStatus, stop: Option<StopReason>) -> Fact {
    let now = now_ms();
    Fact::ModelCalled {
        call: ModelCall {
            message: message.clone(),
            source: QuerySource::MainThread,
            selection: ModelSelection {
                provider: "p".into(),
                model: "m".into(),
                reasoning: Some("high".into()),
                connection: None,
            },
            started_at_ms: now,
            first_token_at_ms: Some(now),
            first_content_at_ms: None,
            completed_at_ms: now,
            status,
            stop_reason: stop,
            tool_calls: 0,
            usage: None,
            retries: 0,
            error: None,
        },
    }
}

fn answer(id: &MessageId, content: Vec<ContentBlock>, stop: Option<StopReason>) -> Fact {
    Fact::MessageAppended {
        message: Message::Assistant(AssistantMessage {
            id: id.clone(),
            content,
            model: RESPONSE_MODEL.into(),
            stop_reason: stop,
            usage: None,
        }),
    }
}

/// 完成的一轮，再一轮在流式响应中被中断：已结束的文本块落库，停止原因为空（query `finish_partial`），
/// 接着是中断消息与回合结束。
async fn interrupted(store: &NodeStore) -> Result<SessionId> {
    let id = create(store).await?;
    let (u1, a1, u2, a2) = (message_id(), message_id(), message_id(), message_id());
    let facts = vec![
        user(&u1, "Say hi"),
        answer(&a1, vec![text("Hi.")], Some(StopReason::EndTurn)),
        called(&a1, CallStatus::Completed, Some(StopReason::EndTurn)),
        Fact::TurnEnded {
            status: CallStatus::Completed,
        },
    ];
    store.commit(&id, batch(Some("cmd_i1"), facts)).await?;
    let facts = vec![
        user(&u2, "Write a long story"),
        answer(&a2, vec![text("Once upon")], None),
        called(&a2, CallStatus::Cancelled, None),
        user_with(
            vec![text("[Request interrupted by user]")],
            UserOrigin::Human,
        ),
        Fact::TurnEnded {
            status: CallStatus::Cancelled,
        },
    ];
    store.commit(&id, batch(Some("cmd_i2"), facts)).await?;
    Ok(id)
}

/// 输入还没提交时被中断（修复线 F37，session-lifecycle.md §3.1）：会话 actor 依次提交的输入（带命令）、当步的
/// attachment、上下文宣布、中断标记与回合结束，每条一批、带回合锚点；没有 assistant（没有发出请求）。
async fn early(store: &NodeStore) -> Result<SessionId> {
    let id = create(store).await?;
    let mut turn = None;
    let mut first = anchored(&mut turn, vec![user(&message_id(), "Fix the build")]);
    first.command = Some(CommandId::new("cmd_e1"));
    store.commit(&id, first).await?;
    for fact in [
        attachment(
            "total_tokens_reminder",
            "<system-reminder>\n<total_tokens>15000000 tokens left</total_tokens>\n</system-reminder>",
            Value::Null,
        ),
        attachment(
            "date",
            "<system-reminder>\nToday's date is 2026-10-20.\n</system-reminder>",
            Value::Null,
        ),
        user_with(
            vec![text("[Request interrupted by user]")],
            UserOrigin::Human,
        ),
        Fact::TurnEnded {
            status: CallStatus::Cancelled,
        },
    ] {
        store.commit(&id, anchored(&mut turn, vec![fact])).await?;
    }
    Ok(id)
}

/// esc 撤回刚发的输入（修复线 F38，session-lifecycle.md §5.5）：完成的一轮，再一条在发请求之前被中断的输入（同
/// `early`），之后会话 actor 的 `RestoreCancelled` 只提交一个 `RevertSet`，目标是这条输入（保留前缀与切点由 node-store
/// 从库里算）。活动分支只剩第一轮。
async fn restored(store: &NodeStore) -> Result<SessionId> {
    let id = create(store).await?;
    let mut turn = None;
    let (u1, a1, u2) = (message_id(), message_id(), message_id());
    let mut first = anchored(
        &mut turn,
        vec![
            user(&u1, "Say hi"),
            reply(&a1, vec![text("Hi.")], StopReason::EndTurn),
        ],
    );
    first.command = Some(CommandId::new("cmd_r1"));
    store.commit(&id, first).await?;
    let mut second = anchored(&mut turn, vec![user(&u2, "Fix the build")]);
    second.command = Some(CommandId::new("cmd_r2"));
    store.commit(&id, second).await?;
    for fact in [
        attachment(
            "date",
            "<system-reminder>\nToday's date is 2026-10-20.\n</system-reminder>",
            Value::Null,
        ),
        user_with(
            vec![text("[Request interrupted by user]")],
            UserOrigin::Human,
        ),
        Fact::TurnEnded {
            status: CallStatus::Cancelled,
        },
    ] {
        store.commit(&id, anchored(&mut turn, vec![fact])).await?;
    }
    let cursor = RevertCursor {
        target: u2.clone(),
        kept: vec![u1, a1],
        cut_after: u2,
        generation: 1,
    };
    let revert = batch(Some("cmd_r3"), vec![Fact::RevertSet { cursor }]);
    store.commit(&id, revert).await?;
    Ok(id)
}

/// bash 模式（tui-input.md §2.2）与 skill 命令（ext.md §2.7）的消息，加一条 `plan_mode_exit` 提醒。
async fn displayed(store: &NodeStore) -> Result<SessionId> {
    let id = create(store).await?;
    let facts = vec![
        user(&message_id(), "<bash-input>ls</bash-input>"),
        user_with(
            vec![text(
                "<bash-stdout>a.txt</bash-stdout><bash-stderr></bash-stderr>",
            )],
            UserOrigin::Human,
        ),
        reply(&message_id(), vec![text("One file.")], StopReason::EndTurn),
    ];
    store.commit(&id, batch(Some("cmd_d1"), facts)).await?;
    let facts = vec![
        user(
            &message_id(),
            "<command-message>review</command-message>\n<command-name>/review</command-name>\n<command-args>main</command-args>",
        ),
        user_with(
            vec![text("Review the branch.")],
            UserOrigin::Meta {
                source: "skill".into(),
            },
        ),
        attachment(
            "plan_mode_exit",
            "<system-reminder>\n## Exited Plan Mode\n\nYou can now make edits.\n</system-reminder>",
            json!({"planExists": false}),
        ),
        reply(&message_id(), vec![text("Reviewed.")], StopReason::EndTurn),
    ];
    store.commit(&id, batch(Some("cmd_d2"), facts)).await?;
    Ok(id)
}

/// 一次性子代理（Explore）：子会话、父会话里的调用与结果（没有 agentId 尾注）。
async fn subagent(store: &NodeStore) -> Result<(SessionId, SessionId)> {
    let id = create(store).await?;
    let (u1, a1, a2) = (message_id(), message_id(), message_id());
    let input = json!({"description": "Look around", "prompt": "Find the entry point",
        "subagent_type": "Explore"});
    let facts = vec![
        user(&u1, "Explore the code"),
        reply(
            &a1,
            vec![call("toolu_agent", "Agent", &input)],
            StopReason::ToolUse,
        ),
    ];
    store.commit(&id, batch(Some("cmd_s1"), facts)).await?;
    let agent = ids().subagent().0.to_string();
    let child = SessionId::new(format!("sess_subagent_{agent}"));
    store
        .create(SessionMeta {
            id: child.clone(),
            kind: SessionKind::Subagent,
            parent: Some(id.clone()),
            workspace: WorkspaceRef {
                environment: EnvironmentId::new(LOCAL_ENVIRONMENT),
                root: EnvPath::new(WORKSPACE),
            },
            title: "Look around".into(),
            trace: TraceId::new(id.as_str()),
            created_at_ms: now_ms(),
        })
        .await?;
    let handback = "Here is the report:\n  main.rs is the entry point.";
    let mut facts = vec![result(&a1, "toolu_agent", vec![said(handback)], false)];
    facts.extend(link(&agent, &child));
    facts.push(reply(&a2, vec![text("Found it.")], StopReason::EndTurn));
    store.commit(&id, batch(None, facts)).await?;
    Ok((id, child))
}

/// 子代理调用与子会话的关联（会话 actor 从 Agent 工具的输出得到，与工具结果同批提交）。
fn link(agent: &str, child: &SessionId) -> Vec<Fact> {
    vec![Fact::SubagentLinked {
        tool_use: ToolUseId::new("toolu_agent"),
        agent: rust_cli_kernel_api::ids::AgentId::new(agent),
        child: child.clone(),
        agent_type: "Explore".into(),
    }]
}

pub(super) async fn write(dir: &str) -> Result<()> {
    let store = store(dir)?;
    let interrupted = interrupted(&store).await?;
    let displayed = displayed(&store).await?;
    let (parent, child) = subagent(&store).await?;
    let early = early(&store).await?;
    let restored = restored(&store).await?;
    print(&json!({
        "interrupted": interrupted,
        "early": early,
        "restored": restored,
        "displayed": displayed,
        "subagent": parent,
        "child": child,
        "responseModel": RESPONSE_MODEL,
    }))
}

/// 一轮 Write：工具结果与两个 checkpoint 同批提交（同一批里的两个 checkpoint 序号也要不同）。
pub(super) async fn checkpoints(dir: &str, session: &str) -> Result<()> {
    let store = store(dir)?;
    let id = SessionId::new(session);
    let (u1, a1) = (message_id(), message_id());
    let input = json!({"file_path": "/w/interop/b.txt", "content": "b\n"});
    let facts = vec![
        user(&u1, "Write b.txt twice"),
        reply(
            &a1,
            vec![
                call("toolu_b1", "Write", &input),
                call("toolu_b2", "Write", &input),
            ],
            StopReason::ToolUse,
        ),
    ];
    store.commit(&id, batch(Some("cmd_c1"), facts)).await?;
    let mut facts = Vec::new();
    for call in ["toolu_b1", "toolu_b2"] {
        let snapshot = store
            .put(
                &id,
                ArtifactPurpose::Checkpoint,
                "application/json".into(),
                b"{}".to_vec(),
            )
            .await?;
        facts.push(result(&a1, call, vec![said("written")], false));
        facts.push(Fact::CheckpointCreated {
            checkpoint: WorkspaceCheckpoint {
                machine: None,
                id: format!("checkpoint_{call}"),
                prompt: u1.clone(),
                assistant: a1.clone(),
                tool_use: ToolUseId::new(call),
                snapshot,
            },
        });
    }
    store.commit(&id, batch(None, facts)).await?;
    print(&json!({"ok": true}))
}

/// Node 群协作会话的续写：“总是允许”一条规则、用户切到 acceptEdits（会话 actor 的两批事实）。
pub(super) async fn scope(dir: &str, session: &str) -> Result<()> {
    let store = store(dir)?;
    let id = SessionId::new(session);
    let remember = Fact::PermissionRulesAdded {
        behavior: RuleBehavior::Allow,
        rules: vec![PermissionRule {
            tool: "Bash".into(),
            content: Some("npm test:*".into()),
        }],
    };
    store.commit(&id, batch(None, vec![remember])).await?;
    let facts = vec![
        Fact::ModeChanged {
            mode: PermissionMode::AcceptEdits,
        },
        Fact::ProjectModeSet {
            mode: PermissionMode::AcceptEdits,
        },
    ];
    store.commit(&id, batch(Some("cmd_scope"), facts)).await?;
    let workspace = WorkspaceRef {
        environment: EnvironmentId::new(LOCAL_ENVIRONMENT),
        root: EnvPath::new(WORKSPACE),
    };
    print(&json!({
        "session": store.session_rules(&id).await?,
        "project": store.project_rules(&workspace).await?,
        "mode": SessionStore::load(&store, &id).await?.mode,
    }))
}

pub(super) async fn task_load(dir: &str, session: &str) -> Result<()> {
    let list = TaskListStore::load(&store(dir)?, &SessionId::new(session)).await?;
    print(&serde_json::to_value(list)?)
}

/// 读清单、加一个任务、存回（TaskCreate 的读改写）。
pub(super) async fn task_add(dir: &str, session: &str, subject: &str) -> Result<()> {
    let store = store(dir)?;
    let id = SessionId::new(session);
    let mut list = TaskListStore::load(&store, &id).await?;
    list.high_water_mark += 1;
    list.tasks.push(TaskRecord {
        id: list.high_water_mark.to_string(),
        subject: subject.into(),
        description: String::new(),
        active_form: None,
        owner: None,
        status: TaskStatus::Pending,
        blocks: Vec::new(),
        blocked_by: Vec::new(),
        metadata: None,
    });
    TaskListStore::save(&store, &id, &list).await?;
    print(&serde_json::to_value(list)?)
}
