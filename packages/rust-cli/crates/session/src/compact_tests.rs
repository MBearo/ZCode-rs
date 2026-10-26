//! `/compact`：作为一个回合运行，边界、摘要与附件一个事务提交（compact.md §4.2）。
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use rust_cli_base::TaskScope;
use rust_cli_kernel_api::compaction::ContextNotice;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{CommandId, EnvironmentId, MessageId, SessionId, TraceId};
use rust_cli_kernel_api::message::{
    CompactBoundary, CompactTrigger, ContentBlock, Message, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::orchestration::{
    CompactRequest, CompactResult, Compactor, OrchestrationError,
};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{
    Ack, Command, CommandEnvelope, KernelEvent, SessionHandle, TurnOutcome,
};
use rust_cli_kernel_api::store::{Fact, SessionKind, SessionMeta, SessionStore};
use rust_cli_kernel_api::testing::{
    MemoryStore, NeverCompact, ScriptedModel, SequentialIds, UnusedEnvironment, local_services,
    machine, selection,
};
use tokio_util::sync::CancellationToken;

use crate::{Opened, Session, SessionDeps, SessionState};

/// 成功时给出固定的结果；`fail` 时以固定的原文失败。
struct FakeCompactor {
    fail: Option<String>,
}

#[async_trait]
impl Compactor for FakeCompactor {
    fn should_auto_compact(&self, _history: &[Message], _selection: &ModelSelection) -> bool {
        false
    }

    /// 历史里有压缩边界时不显示，否则是快满：看得出 actor 用的是提交压缩结果之后的历史。
    fn context_notice(
        &self,
        history: &[Message],
        _selection: &ModelSelection,
    ) -> Option<ContextNotice> {
        let compacted = history.iter().any(|message| {
            matches!(
                message,
                Message::System(rust_cli_kernel_api::message::SystemMessage::CompactBoundary(_))
            )
        });
        Some(if compacted {
            ContextNotice::Hidden
        } else {
            ContextNotice::UntilAutoCompact { percent: 1 }
        })
    }

    /// 用量的 token 是消息数、窗口随模型变（`big` 是 1000，其余 100）：看得出 actor 用的是提交之后的历史与当前模型
    /// （F27）。
    fn context_usage(&self, history: &[Message], selection: &ModelSelection) -> Option<(u64, u64)> {
        let window = if selection.model == "big" { 1_000 } else { 100 };
        Some((history.len() as u64, window))
    }

    async fn compact(
        &self,
        request: CompactRequest,
        _cancel: CancellationToken,
    ) -> Result<CompactResult, OrchestrationError> {
        if let Some(message) = &self.fail {
            return Err(OrchestrationError::Failed {
                message: message.clone(),
            });
        }
        assert_eq!(request.trigger, CompactTrigger::Manual);
        assert_eq!(request.instructions.as_deref(), Some("focus"));
        // 冷加载的历史以没有回答的输入结束：补了一条 assistant（session-lifecycle.md §6.3.1）。
        assert_eq!(request.messages.len(), 2);
        Ok(CompactResult {
            boundary: CompactBoundary {
                id: MessageId::new("msg_b"),
                trigger: CompactTrigger::Manual,
                pre_tokens: 5,
                summary: MessageId::new("msg_s"),
                preserved: None,
                discovered_tools: Vec::new(),
            },
            summary: UserMessage {
                id: MessageId::new("msg_s"),
                content: vec![ContentBlock::Text {
                    text: "S".to_owned(),
                }],
                origin: UserOrigin::CompactSummary,
            },
            restored: Vec::new(),
        })
    }
}

fn meta() -> SessionMeta {
    SessionMeta {
        id: SessionId::new("sess_1"),
        kind: SessionKind::Interactive,
        parent: None,
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/work"),
        },
        title: String::new(),
        trace: TraceId::new("trace_1"),
        created_at_ms: 0,
    }
}

async fn session(fail: Option<&str>) -> (TaskScope, Arc<MemoryStore>, Session) {
    let scope = TaskScope::root("test", |_| {});
    let store = Arc::new(MemoryStore::default());
    store.create(meta()).await.unwrap();
    let env = Arc::new(UnusedEnvironment);
    let runner = Arc::new(rust_cli_query::QueryLoop::new(rust_cli_query::QueryParts {
        model: Arc::new(ScriptedModel::new(Vec::new())),
        context: Arc::new(rust_cli_kernel_api::testing::FixedContext(Vec::new())),
        tools: Arc::new(rust_cli_kernel_api::testing::NoTools),
        executor: Arc::new(rust_cli_kernel_api::testing::FailingExecutor),
        compactor: Arc::new(NeverCompact),
        hooks: Arc::new(rust_cli_kernel_api::testing::NoHooks),
        ids: Arc::new(SequentialIds::default()),
        clock: Arc::new(rust_cli_kernel_api::testing::FixedClock(0)),
        machines: rust_cli_kernel_api::testing::environments(env.clone()),
        machine: machine(env.clone()),
        local: local_services(env),
        artifacts: store.clone(),
        generation: rust_cli_kernel_api::ids::Generation::new("g"),
        non_interactive: false,
        limits: rust_cli_query::RequestLimits::default(),
    }));
    let history = vec![Message::User(UserMessage {
        id: MessageId::new("msg_u"),
        content: vec![ContentBlock::Text {
            text: "hi".to_owned(),
        }],
        origin: UserOrigin::Human,
    })];
    let env = Arc::new(UnusedEnvironment);
    let session = Session::spawn(
        SessionDeps {
            agents: Arc::new(rust_cli_kernel_api::agents::NoAgentSignals),
            cron: Arc::new(rust_cli_kernel_api::cron::NoCron),
            store: store.clone(),
            ids: Arc::new(SequentialIds::default()),
            runner,
            compactor: Arc::new(FakeCompactor {
                fail: fail.map(str::to_owned),
            }),
            machines: rust_cli_kernel_api::testing::environments(env.clone()),
            machine: machine(env.clone()),
            local: local_services(env),
            max_turns: None,
            hooks: Arc::new(rust_cli_kernel_api::testing::NoHooks),
            commands: Arc::new(rust_cli_kernel_api::user_command::NoUserCommands),
            inputs: Arc::new(rust_cli_kernel_api::user_input::NoUserInputs),
            artifacts: Arc::new(rust_cli_kernel_api::testing::MemoryArtifacts::default()),
            clock: Arc::new(rust_cli_kernel_api::testing::FixedClock(0)),
            generation: rust_cli_kernel_api::ids::Generation::new("g1"),
            spawner: scope.spawner(),
            closing: scope.spawner(),
            titles: None,
            side_model: Arc::new(rust_cli_kernel_api::testing::ScriptedModel::new(Vec::new())),
            released: Arc::new(crate::NothingToRelease),
            directory_rules: Arc::new(rust_cli_kernel_api::orchestration::NoDirectoryRules),
            peers: crate::PeerDeps::off(),
            detached: None,
        },
        SessionState {
            meta: meta(),
            messages: history,
            earlier: Vec::new(),
            selection: selection(),
            mode: PermissionMode::Default.into(),
            checkpoints: Vec::new(),
            start: rust_cli_kernel_api::session_api::StartSource::Startup,
            turn: None,
            last_turn_failed: false,
        },
        Opened::Loaded,
    );
    (scope, store, session)
}

fn compact() -> CommandEnvelope {
    CommandEnvelope {
        id: CommandId::new("cmd_compact"),
        command: Command::Compact {
            instructions: Some("focus".to_owned()),
        },
    }
}

async fn events_until_finished(session: &Session) -> Vec<KernelEvent> {
    let mut events = session.subscribe(Some(0));
    let mut seen = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), events.next())
            .await
            .unwrap()
            .unwrap()
            .event;
        let done = matches!(event, KernelEvent::TurnFinished { .. });
        seen.push(event);
        if done {
            return seen;
        }
    }
}

#[tokio::test]
async fn compact_runs_as_a_turn_and_commits_one_batch() {
    let (_scope, store, session) = session(None).await;
    assert!(matches!(
        session.send(compact()).await,
        Ack::Accepted { .. }
    ));
    let events = events_until_finished(&session).await;
    let kinds: Vec<String> = events
        .iter()
        .map(|event| {
            serde_json::to_value(event).unwrap()["type"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "turn_started",
            "compact_started",
            "message_done",
            "message_done",
            // 修复线 L-A：`/compact` 的命令记录（固定说明、命令输入、输出）同一批，排在摘要之后（tui-manage.md §11.2）。
            "message_done",
            "message_done",
            "message_done",
            "compact_done",
            "context_notice",
            "context_usage",
            "turn_finished"
        ]
    );
    // F14：压缩结果提交后就重算上下文提示（消息变化时重算），不等下一条回复。
    assert!(events.contains(&KernelEvent::ContextNotice {
        notice: ContextNotice::Hidden
    }));
    // F27：footer 的 ctx 同一处重算，用的是提交之后的历史（原有的 1 条、冷加载补上的 assistant，加边界与摘要；
    // 修复线 L-A 起另有 `/compact` 记录的 3 条）。
    assert!(events.contains(&KernelEvent::ContextUsage {
        used_tokens: 7,
        max_tokens: 100
    }));
    assert!(matches!(
        events.last(),
        Some(KernelEvent::TurnFinished {
            outcome: TurnOutcome::Success,
            ..
        })
    ));
    let facts = store.facts(&SessionId::new("sess_1"));
    let appended: Vec<&str> = facts
        .iter()
        .filter_map(|fact| match fact {
            Fact::MessageAppended { message } => Some(message.id().as_str()),
            _ => None,
        })
        .collect();
    // 冷加载补的 assistant（msg_1）随这一批落库，排在最前面（工作线 F77，session-lifecycle.md §6.3.1）。
    assert_eq!(appended[..3], ["msg_1", "msg_b", "msg_s"]);
    let view = session.view().await.unwrap();
    assert_eq!(view.messages.len(), 7);
    assert_eq!(
        record_texts(&view.messages),
        [
            rust_cli_kernel_api::user_input::BASH_CAVEAT.to_owned(),
            "<command-name>/compact</command-name>\n            <command-message>compact</command-message>\n            <command-args>focus</command-args>".to_owned(),
            "<local-command-stdout>\u{1b}[2mCompacted (ctrl+o to see full summary)\u{1b}[22m</local-command-stdout>".to_owned(),
        ]
    );
}

/// 本地命令记录的文字（来源以 `local-command` 开头的 meta 消息）。
fn record_texts(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|message| match message {
            Message::User(user)
                if matches!(&user.origin, rust_cli_kernel_api::message::UserOrigin::Meta { source } if source.starts_with("local-command")) =>
            {
                match user.content.as_slice() {
                    [rust_cli_kernel_api::message::ContentBlock::Text { text }] => Some(text.clone()),
                    _ => None,
                }
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_failed_compaction_finishes_the_turn_with_the_reason() {
    let (_scope, _store, session) = session(Some("Not enough messages to compact.")).await;
    assert!(matches!(
        session.send(compact()).await,
        Ack::Accepted { .. }
    ));
    let events = events_until_finished(&session).await;
    assert_eq!(
        events.last(),
        Some(&KernelEvent::TurnFinished {
            turn: match &events[0] {
                KernelEvent::TurnStarted { turn, .. } => turn.clone(),
                other => panic!("{other:?}"),
            },
            outcome: TurnOutcome::Failed {
                message: "Not enough messages to compact.".to_owned()
            },
            usage: None,
        })
    );
    // 修复线 L-A：失败也记，输出是 stderr（对照实现的 local 命令抛错时同样）。
    let view = session.view().await.unwrap();
    assert_eq!(
        record_texts(&view.messages)[2],
        "<local-command-stderr>Not enough messages to compact.</local-command-stderr>"
    );
}

/// F27：快照带上下文用量与提示（冷加载后不用等下一条回复，compact.md §2.1）；修前是空的。
#[tokio::test]
async fn the_view_carries_the_context_usage_and_notice() {
    let (_scope, _store, session) = session(None).await;
    let view = session.view().await.unwrap();
    // 消息数：冷加载的一条输入与补上的 assistant（session-lifecycle.md §6.3.1）。
    assert_eq!(
        view.context_usage,
        Some(rust_cli_kernel_api::compaction::ContextOccupancy {
            used_tokens: 2,
            max_tokens: 100
        })
    );
    assert_eq!(
        view.context_notice,
        ContextNotice::UntilAutoCompact { percent: 1 }
    );
}

/// F27：换模型后按新模型的窗口重算并发出（窗口随模型变）；修前要等下一条回复。
#[tokio::test]
async fn changing_the_model_recomputes_the_context() {
    let (_scope, _store, session) = session(None).await;
    let big = ModelSelection {
        model: "big".to_owned(),
        ..selection()
    };
    let ack = session
        .send(CommandEnvelope {
            id: CommandId::new("cmd_model"),
            command: Command::SetModel { selection: big },
        })
        .await;
    assert!(matches!(ack, Ack::Accepted { .. }), "{ack:?}");
    let mut events = session.subscribe(Some(0));
    let mut kinds = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), events.next())
            .await
            .unwrap()
            .unwrap()
            .event;
        let done = matches!(event, KernelEvent::ContextUsage { .. });
        kinds.push(event);
        if done {
            break;
        }
    }
    assert!(
        matches!(kinds[0], KernelEvent::ModelChanged { .. }),
        "{kinds:?}"
    );
    assert_eq!(
        kinds[1..],
        [
            KernelEvent::ContextNotice {
                notice: ContextNotice::UntilAutoCompact { percent: 1 }
            },
            KernelEvent::ContextUsage {
                used_tokens: 2,
                max_tokens: 1_000
            }
        ]
    );
}
