//! 权限询问的端到端（LocalKernel + 真实执行管线 + 会话 actor）：被取消的询问（R1-5）与“总是允许”的唯一来源
//! （R1-2）。docs/specs/rust-cli/shell-permissions.md §5、session-lifecycle.md §2。

// 集成测试里的辅助函数也按测试代码处理（clippy 的 allow-unwrap-in-tests 只认 cfg(test)）。
#[cfg(test)]
mod interactions {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex, PoisonError};
    use std::time::Duration;

    use async_trait::async_trait;
    use futures_util::StreamExt;
    use futures_util::stream::BoxStream;
    use rust_cli_kernel_api::env::{
        CommandEvent, EnvError, EnvFacts, EnvPath, Environment, FileContent, OperationContext,
        Outcome, ReadFile, StartCommand, WorkspaceRef, WriteFile, WriteReceipt,
    };
    use rust_cli_kernel_api::ids::{
        CommandId, EnvironmentId, InteractionId, OperationId, SessionId,
    };
    use rust_cli_kernel_api::message::{ContentBlock, Message, StopReason, Usage};
    use rust_cli_kernel_api::model::{BlockDelta, ModelError, ModelEvent, ModelPort, ModelRequest};
    use rust_cli_kernel_api::orchestration::InteractionRequest;
    use rust_cli_kernel_api::permission::PermissionMode;
    use rust_cli_kernel_api::session_api::{
        Ack, Command, CommandEnvelope, CreateSession, Delivery, Kernel, KernelEvent,
        SequencedEvent, SessionHandle,
    };
    use rust_cli_kernel_api::store::Fact;
    use rust_cli_kernel_api::testing::{
        FixedClock, MemoryStore, SequentialIds, selection, text_response, tool_use_response,
    };
    use rust_cli_runtime::{Defaults, LocalKernel, Ports};
    use serde_json::{Value, json};
    use tokio::sync::Notify;
    use tokio_util::sync::CancellationToken;

    type Events = Vec<Result<ModelEvent, ModelError>>;

    /// 按顺序给出响应；带闸门的响应等测试放行后才流出。
    #[derive(Default)]
    struct Script(Mutex<VecDeque<(Option<Arc<Notify>>, Events)>>);

    impl Script {
        fn new(responses: Vec<(Option<Arc<Notify>>, Events)>) -> Self {
            Self(Mutex::new(responses.into()))
        }
    }

    impl ModelPort for Script {
        fn stream(
            &self,
            _request: ModelRequest,
            _cancel: CancellationToken,
        ) -> BoxStream<'static, Result<ModelEvent, ModelError>> {
            let next = self
                .0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pop_front();
            let Some((gate, events)) = next else {
                return futures_util::stream::iter(text_response(&["done"])).boxed();
            };
            futures_util::stream::once(async move {
                if let Some(gate) = gate {
                    gate.notified().await;
                }
                futures_util::stream::iter(events)
            })
            .flatten()
            .boxed()
        }
    }

    /// 一次响应里的两个工具调用（并发安全的两个调用同时开工）。
    fn two_calls(first: (&str, &str, Value), second: (&str, &str, Value)) -> Events {
        let block = |index: usize, (id, name, input): (&str, &str, Value)| {
            let one = tool_use_response(id, name, &input);
            // tool_use_response 的第 1–3 项是这个块的开始、输入与结束；换成第 `index` 个块。
            one.into_iter()
                .skip(1)
                .take(3)
                .map(move |event| match event {
                    Ok(ModelEvent::BlockStart { block, .. }) => {
                        Ok(ModelEvent::BlockStart { index, block })
                    }
                    Ok(ModelEvent::BlockDelta { delta, .. }) => {
                        Ok(ModelEvent::BlockDelta { index, delta })
                    }
                    Ok(ModelEvent::BlockStop { .. }) => Ok(ModelEvent::BlockStop { index }),
                    other => other,
                })
                .collect::<Vec<_>>()
        };
        let mut events = vec![Ok(ModelEvent::MessageStart {
            id: "msg_api".to_owned(),
            model: "scripted".to_owned(),
            usage: Usage::default(),
        })];
        events.extend(block(0, first));
        events.extend(block(1, second));
        events.extend([
            Ok(ModelEvent::MessageDelta {
                stop_reason: Some(StopReason::ToolUse),
                usage: Usage::default(),
            }),
            Ok(ModelEvent::MessageStop),
        ]);
        let _ = BlockDelta::Text(String::new());
        events
    }

    /// 命令在测试放行后才失败的执行环境（让并发的 Bash 在询问登记之后出错、取消兄弟调用）；其余操作不支持。
    struct GatedEnv {
        gate: Arc<Notify>,
    }

    fn unsupported() -> EnvError {
        EnvError::Unsupported {
            what: "test environment".to_owned(),
        }
    }

    #[async_trait]
    impl Environment for GatedEnv {
        async fn facts(&self, _workspace: &WorkspaceRef) -> Result<EnvFacts, EnvError> {
            Err(unsupported())
        }

        async fn read_file(
            &self,
            _workspace: &WorkspaceRef,
            _request: ReadFile,
        ) -> Result<FileContent, EnvError> {
            Err(unsupported())
        }

        async fn write_file(
            &self,
            _workspace: &WorkspaceRef,
            _context: OperationContext,
            _request: WriteFile,
        ) -> Result<WriteReceipt, EnvError> {
            Err(unsupported())
        }

        async fn start_command(
            &self,
            _workspace: &WorkspaceRef,
            _context: OperationContext,
            _request: StartCommand,
        ) -> Result<(), EnvError> {
            self.gate.notified().await;
            Err(unsupported())
        }

        fn watch_command(
            &self,
            _operation: OperationId,
            _cursor: u64,
        ) -> BoxStream<'static, Result<CommandEvent, EnvError>> {
            futures_util::stream::iter([Err(unsupported())]).boxed()
        }

        async fn cancel(&self, _operation: OperationId) -> Result<(), EnvError> {
            Err(unsupported())
        }

        async fn operation_status(&self, _operation: OperationId) -> Result<Outcome, EnvError> {
            Ok(Outcome::Unknown)
        }

        async fn close_session(&self, _session: &SessionId, _forget: bool) -> Result<(), EnvError> {
            Ok(())
        }
    }

    fn workspace() -> WorkspaceRef {
        WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/work"),
        }
    }

    fn kernel(
        model: Arc<dyn ModelPort>,
        env: Arc<dyn Environment>,
        store: Arc<MemoryStore>,
    ) -> LocalKernel {
        LocalKernel::new(
            Ports {
                model,
                env,
                store: store.clone(),
                artifacts: store,
                clock: Arc::new(FixedClock(1_000)),
                ids: Arc::new(SequentialIds::default()),
            },
            Defaults {
                selection: selection(),
                mode: PermissionMode::Default,
                max_turns: None,
                search_tools: false,
                task_tools: None,
                limits: Default::default(),
            },
        )
    }

    async fn open(kernel: &LocalKernel) -> Arc<dyn SessionHandle> {
        let id = kernel
            .create_session(CreateSession {
                id: None,
                workspace: workspace(),
                selection: selection(),
                mode: PermissionMode::Default,
                plan: false,
            })
            .await
            .unwrap();
        kernel.open_session(&id).await.unwrap()
    }

    fn submit(id: &str, text: &str) -> CommandEnvelope {
        CommandEnvelope {
            id: CommandId::new(id),
            command: Command::Submit {
                text: text.to_owned(),
                attachments: Vec::new(),
                delivery: Delivery::Queue,
            },
        }
    }

    /// 读事件直到满足 `done`（10 s 内，只为让失败的测试结束）。
    async fn until(
        events: &mut BoxStream<'static, SequencedEvent>,
        done: impl Fn(&KernelEvent) -> bool,
    ) -> Vec<KernelEvent> {
        let mut seen = Vec::new();
        loop {
            let event = tokio::time::timeout(Duration::from_secs(10), events.next())
                .await
                .unwrap_or_else(|_| panic!("timed out; saw {seen:?}"))
                .unwrap();
            let stop = done(&event.event);
            seen.push(event.event);
            if stop {
                return seen;
            }
        }
    }

    fn permission_asked(event: &KernelEvent) -> Option<InteractionId> {
        match event {
            KernelEvent::InteractionAsked {
                interaction,
                request: InteractionRequest::Permission { .. },
            } => Some(interaction.clone()),
            _ => None,
        }
    }

    /// 复现（R1-5）：工具被取消时（这里是用户中断），以前只丢掉等回答的 future——actor 的待回答交互要到回合
    /// 结束才清：对话框一直显示，之后到达的“总是允许”照样提交规则。现在取消时撤回询问：工具结束之前对话框
    /// 已关，迟到的回答被拒绝，不写规则。原来用“Bash 出错取消兄弟调用”触发取消；现在没有这条
    /// 规则（R4 F3），改用用户中断。
    #[tokio::test(flavor = "multi_thread")]
    async fn a_cancelled_tool_withdraws_its_permission_prompt() {
        let failing = Arc::new(Notify::new());
        let reply = Arc::new(Notify::new());
        let store = Arc::new(MemoryStore::default());
        let model = Arc::new(Script::new(vec![
            (
                None,
                two_calls(
                    ("toolu_read", "Read", json!({"file_path": "/etc/hosts"})),
                    ("toolu_bash", "Bash", json!({"command": "ls"})),
                ),
            ),
            (Some(reply.clone()), text_response(&["after the tools"])),
        ]));
        let env = Arc::new(GatedEnv {
            gate: failing.clone(),
        });
        let kernel = kernel(model, env, store.clone());
        let session = open(&kernel).await;
        let mut events = session.subscribe(Some(0));
        session.send(submit("c1", "read the hosts file")).await;
        let seen = until(&mut events, |event| permission_asked(event).is_some()).await;
        let interaction = seen.iter().find_map(permission_asked).unwrap();
        // 用户中断 → 等回答的 Read 被取消，它的询问随之撤回。
        session
            .send(CommandEnvelope {
                id: CommandId::new("i1"),
                command: Command::Interrupt,
            })
            .await;
        // 中断时被拒绝的调用直接交付拒绝结果（没有 ToolDone）：等它的结果消息。
        let seen = until(&mut events, |event| match event {
            KernelEvent::MessageDone {
                message: Message::User(user),
            } => user.content.iter().any(|block| {
                matches!(block, ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id.as_str() == "toolu_read")
            }),
            _ => false,
        })
        .await;
        assert!(
            seen.iter().any(|event| matches!(
                event,
                KernelEvent::InteractionResolved { interaction: resolved } if *resolved == interaction
            )),
            "the prompt must close before the cancelled tool ends: {seen:?}"
        );
        assert!(session.view().await.unwrap().pending.is_empty());
        // 迟到的“总是允许”：被拒绝，不写规则。
        let late = session
            .send(CommandEnvelope {
                id: CommandId::new("c2"),
                command: Command::ResolveInteraction {
                    interaction,
                    answer: json!({"decision": "allow", "remember": [{"tool": "Read", "content": "//etc/**"}]}),
                },
            })
            .await;
        assert!(matches!(late, Ack::Rejected { .. }), "{late:?}");
        let facts = store.facts(session.id());
        assert!(
            !facts
                .iter()
                .any(|fact| matches!(fact, Fact::PermissionRulesAdded { .. })),
            "a withdrawn prompt must not store an always-allow rule"
        );
        // 放行被门挡住的 Bash，回合随中断结束。
        failing.notify_one();
        reply.notify_one();
        until(&mut events, |event| {
            matches!(event, KernelEvent::TurnFinished { .. })
        })
        .await;
    }

    fn permission_suggestions(event: &KernelEvent) -> Option<(InteractionId, Value)> {
        match event {
            KernelEvent::InteractionAsked {
                interaction,
                request: InteractionRequest::Permission { suggestions, .. },
            } => Some((interaction.clone(), json!(suggestions))),
            _ => None,
        }
    }

    /// 复现（R1-2）：“总是允许”以前库里一份、判决链按会话一份（`PolicyGate.session_rules`），判决链的
    /// LocalSettings 层不刷新——`/clear` 之后的新会话与子代理（都是别的会话 ID）又问一次。现在回答之后按
    /// `/permissions` 的路径读回库值、换上 LocalSettings 层，进程里所有会话共用一份权限上下文。
    #[tokio::test(flavor = "multi_thread")]
    async fn an_always_allow_answer_reaches_the_next_session() {
        let read = || tool_use_response("toolu_read", "Read", &json!({"file_path": "/etc/hosts"}));
        let store = Arc::new(MemoryStore::default());
        let model = Arc::new(Script::new(vec![
            (None, read()),
            (None, text_response(&["first done"])),
            (None, read()),
            (None, text_response(&["second done"])),
        ]));
        let kernel = kernel(
            model,
            Arc::new(rust_cli_kernel_api::testing::UnusedEnvironment),
            store.clone(),
        );
        let first = open(&kernel).await;
        let mut events = first.subscribe(Some(0));
        first.send(submit("c1", "read the hosts file")).await;
        let seen = until(&mut events, |event| permission_suggestions(event).is_some()).await;
        let (interaction, _) = seen.iter().find_map(permission_suggestions).unwrap();
        // 用户选“允许并记住”：规则是绝对路径的写法（`//` 开头）。
        let answer = first
            .send(CommandEnvelope {
                id: CommandId::new("c2"),
                command: Command::ResolveInteraction {
                    interaction,
                    answer: json!({"decision": "allow", "remember": [{"tool": "Read", "content": "//etc/**"}]}),
                },
            })
            .await;
        assert!(matches!(answer, Ack::Accepted { .. }), "{answer:?}");
        until(&mut events, |event| {
            matches!(event, KernelEvent::TurnFinished { .. })
        })
        .await;

        // `/clear` 之后的新会话（另一个会话 ID）：同一个调用不再询问。
        let second = open(&kernel).await;
        let mut events = second.subscribe(Some(0));
        second.send(submit("c3", "read it again")).await;
        let seen = until(&mut events, |event| {
            matches!(
                event,
                KernelEvent::TurnFinished { .. } | KernelEvent::InteractionAsked { .. }
            )
        })
        .await;
        assert!(
            matches!(seen.last(), Some(KernelEvent::TurnFinished { .. })),
            "the remembered rule must apply to the next session: {:?}",
            seen.last()
        );
    }
}
