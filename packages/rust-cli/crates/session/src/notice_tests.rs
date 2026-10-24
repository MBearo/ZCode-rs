//! 后台命令的终态通知：空闲时开新回合；运行中在下一个工具边界并入（L18）；提交后才 ack；重复送达只
//! ack（docs/specs/rust-cli/query.md §5）。

use rust_cli_kernel_api::ids::{MessageId, ShellTaskId};
use rust_cli_kernel_api::message::{ContentBlock, Message, UserMessage, UserOrigin};
use rust_cli_kernel_api::testing::{
    DirectExecutor, ListTools, NoticeEnvironment, local_services, machine, shell_notice,
    tool_use_response,
};
use rust_cli_kernel_api::tool::Tool;
use rust_cli_query::task_notification;

use super::*;

pub(super) async fn session(
    scope: &TaskScope,
    env: Arc<NoticeEnvironment>,
    model: Arc<dyn ModelPort>,
    tools: Vec<Arc<dyn Tool>>,
    messages: Vec<Message>,
) -> Session {
    session_on(
        scope,
        env,
        model,
        tools,
        messages,
        Arc::new(rust_cli_kernel_api::testing::FixedClock(0)),
    )
    .await
}

/// 同 [`session`]，执行环境可以是任何替身（monitors_tests 用自己的）。
pub(crate) async fn session_on<E: rust_cli_kernel_api::testing::AllPorts + 'static>(
    scope: &TaskScope,
    env: Arc<E>,
    model: Arc<dyn ModelPort>,
    tools: Vec<Arc<dyn Tool>>,
    messages: Vec<Message>,
    clock: Arc<dyn rust_cli_kernel_api::clock::Clock>,
) -> Session {
    let store = Arc::new(MemoryStore::default());
    store.create(meta()).await.unwrap();
    let ids = Arc::new(SequentialIds::default());
    let runner = Arc::new(QueryLoop::new(QueryParts {
        model,
        context: Arc::new(FixedContext(Vec::new())),
        tools: Arc::new(ListTools(tools)),
        executor: Arc::new(DirectExecutor),
        compactor: Arc::new(NeverCompact),
        hooks: Arc::new(NoHooks),
        ids: ids.clone(),
        clock: Arc::new(rust_cli_kernel_api::testing::FixedClock(0)),
        machine: machine(env.clone()),
        local: local_services(env.clone()),
        artifacts: store.clone(),
        generation: Generation::new("g1"),
        non_interactive: false,
        limits: rust_cli_query::RequestLimits::default(),
    }));
    Session::spawn(
        SessionDeps {
            agents: Arc::new(rust_cli_kernel_api::agents::NoAgentSignals),
            cron: Arc::new(rust_cli_kernel_api::cron::NoCron),
            store,
            ids,
            runner,
            compactor: Arc::new(NeverCompact),
            machine: machine(env.clone()),
            local: local_services(env),
            max_turns: None,
            hooks: Arc::new(rust_cli_kernel_api::testing::NoHooks),
            commands: Arc::new(rust_cli_kernel_api::user_command::NoUserCommands),
            inputs: Arc::new(rust_cli_kernel_api::user_input::NoUserInputs),
            artifacts: Arc::new(rust_cli_kernel_api::testing::MemoryArtifacts::default()),
            clock,
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
            messages,
            selection: selection(),
            mode: PermissionMode::Default.into(),
            checkpoints: Vec::new(),
            start: rust_cli_kernel_api::session_api::StartSource::Startup,
            turn: None,
            last_turn_failed: false,
        },
        Opened::Loaded,
    )
}

pub(super) fn notices_in(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|message| match message {
            Message::User(user)
                if user.origin
                    == UserOrigin::Meta {
                        source: "task-notification".to_owned(),
                    } =>
            {
                Some(format!("{:?}", user.content))
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn an_idle_session_starts_a_turn_for_a_notice_and_acks_after_the_commit() {
    let scope = TaskScope::root("test", |_| {});
    let env = Arc::new(NoticeEnvironment::default());
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["seen"])]));
    let session = session(&scope, env.clone(), model.clone(), Vec::new(), Vec::new()).await;
    let mut events = session.subscribe(Some(0));
    env.finish(shell_notice(1, "b1"));
    let seen = until_finished(&mut events, 1).await;
    assert!(seen.iter().any(|event| matches!(
        event,
        KernelEvent::BackgroundWork { work, status } if work == "b1" && status == "completed"
    )));
    assert!(
        seen.iter()
            .any(|event| matches!(event, KernelEvent::TurnStarted { command: None, .. }))
    );
    assert_eq!(env.acked(), [1]);
    let view = session.view().await.unwrap();
    assert!(!view.running && view.background.is_empty());
    assert_eq!(notices_in(&view.messages).len(), 1);
    // 通知原文作为 user 消息发给模型。
    let text = task_notification(&shell_notice(1, "b1"));
    assert!(format!("{:?}", model.requests()[0].messages).contains(&format!("{text:?}")));
}

/// L18：运行中到达的通知在下一个工具边界并入（优先级 `next`），排在之后提交的 `Queue`
/// 输入之前；以前是“等回合结束、排在排队输入之后”。
#[tokio::test]
async fn a_notice_during_a_turn_joins_it_before_queued_input() {
    let scope = TaskScope::root("test", |_| {});
    let env = Arc::new(NoticeEnvironment::default());
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response(
            "toolu_1",
            "AskUserQuestion",
            &serde_json::json!({"question": "which?"}),
        ),
        text_response(&["thanks"]),
        text_response(&["queued reply"]),
    ]));
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(AskingTool)];
    let session = session(&scope, env.clone(), model.clone(), tools, Vec::new()).await;
    let mut events = session.subscribe(Some(0));
    session.send(submit("c1", "ask me")).await;
    let interaction = asked(&mut events).await;
    env.finish(shell_notice(1, "b1"));
    session.send(submit("c2", "next turn")).await;
    let view = session.view().await.unwrap();
    // 回合还在跑：通知不开新回合、不 ack，仍算后台工作。
    assert!(view.running);
    assert_eq!(view.background, [ShellTaskId::new("b1")]);
    assert!(env.acked().is_empty());
    session.send(answer("c3", interaction)).await;
    until_finished(&mut events, 2).await;
    let requests = model.requests();
    assert_eq!(requests.len(), 3);
    assert!(format!("{:?}", requests[1].messages).contains("<task-id>b1</task-id>"));
    assert!(format!("{:?}", requests[2].messages.last()).contains("next turn"));
    assert_eq!(env.acked(), [1]);
}

#[tokio::test]
async fn a_notice_that_is_already_in_the_conversation_is_only_acked() {
    let scope = TaskScope::root("test", |_| {});
    let env = Arc::new(NoticeEnvironment::default());
    let model = Arc::new(ScriptedModel::new(Vec::new()));
    let delivered = Message::User(UserMessage {
        id: MessageId::new("msg_notice"),
        content: vec![ContentBlock::Text {
            text: task_notification(&shell_notice(1, "b1")),
        }],
        origin: UserOrigin::Meta {
            source: "task-notification".to_owned(),
        },
    });
    let session = session(
        &scope,
        env.clone(),
        model.clone(),
        Vec::new(),
        vec![delivered],
    )
    .await;
    env.finish(shell_notice(1, "b1"));
    // 等 actor 处理完这条通知：ack 之后后台列表变空。
    tokio::time::timeout(Duration::from_secs(5), async {
        while !session.view().await.unwrap().background.is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(env.acked(), [1]);
    assert!(model.requests().is_empty());
}

/// L18 的通道顺序与 L22 的斜杠命令规则一起成立：同一个工具边界并入通知与普通插话，斜杠命令留在通道里，
/// 回合结束后作为独立回合（query.md §4、§5）。
#[tokio::test]
async fn a_slash_steer_waits_while_a_notice_and_a_plain_steer_join() {
    let scope = TaskScope::root("test", |_| {});
    let env = Arc::new(NoticeEnvironment::default());
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response(
            "toolu_1",
            "AskUserQuestion",
            &serde_json::json!({"question": "which?"}),
        ),
        text_response(&["thanks"]),
        text_response(&["greeted"]),
    ]));
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(AskingTool)];
    let session = session(&scope, env.clone(), model.clone(), tools, Vec::new()).await;
    let mut events = session.subscribe(Some(0));
    session.send(submit("c1", "ask me")).await;
    let interaction = asked(&mut events).await;
    let steer = |id: &str, text: &str| CommandEnvelope {
        id: CommandId::new(id),
        command: Command::Submit {
            text: text.to_owned(),
            attachments: Vec::new(),
            delivery: Delivery::Steer,
        },
    };
    session.send(steer("c2", "/greet Ada")).await;
    env.finish(shell_notice(1, "b1"));
    session.send(steer("c3", "also b")).await;
    session.send(answer("c4", interaction)).await;
    until_finished(&mut events, 2).await;
    let requests = model.requests();
    assert_eq!(requests.len(), 3);
    let second = format!("{:?}", requests[1].messages);
    assert!(
        second.contains("<task-id>b1</task-id>") && second.contains("also b"),
        "{second}"
    );
    assert!(!second.contains("/greet Ada"), "{second}");
    assert!(format!("{:?}", requests[2].messages.last()).contains("/greet Ada"));
    assert_eq!(env.acked(), [1]);
}
