//! 子代理的权限询问（docs/specs/rust-cli/agents.md §4.1.1，修复线 F67）：LocalKernel + 真实主循环 + agents + 执行管线。
//! 交互界面里后台子代理的询问不随父回合结束，经父会话登记、回答；`-p` 的后台子代理不询问。

// 集成测试里的辅助函数也按测试代码处理（clippy 的 allow-unwrap-in-tests 只认 cfg(test)）。
#[cfg(test)]
mod background_asks {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use futures_util::stream::{self, BoxStream, StreamExt};
    use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
    use rust_cli_kernel_api::ids::{CommandId, EnvironmentId, InteractionId};
    use rust_cli_kernel_api::message::{ContentBlock, ToolResultContent};
    use rust_cli_kernel_api::model::{
        ModelError, ModelEvent, ModelPort, ModelRequest, QuerySource,
    };
    use rust_cli_kernel_api::orchestration::InteractionRequest;
    use rust_cli_kernel_api::permission::PermissionMode;
    use rust_cli_kernel_api::session_api::{
        Ack, Command, CommandEnvelope, CreateSession, Delivery, EndReason, Kernel, KernelEvent,
        SequencedEvent, SessionHandle,
    };
    use rust_cli_kernel_api::testing::{
        FixedClock, MemoryStore, SequentialIds, UnusedEnvironment, environments, selection,
        text_response, tool_use_response,
    };
    use rust_cli_runtime::{ContextConfig, Defaults, LocalKernel, Ports, Surface};
    use serde_json::{Value, json};
    use tokio::sync::watch;
    use tokio_util::sync::CancellationToken;

    type Item = Result<ModelEvent, ModelError>;

    const SUBAGENT_DENIED: &str = "Permission for this tool use was denied. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). Try a different approach or report the limitation to complete your task.";

    /// 主线程按序取剧本（用完回 `Noted.`）；子代理第一步调 Bash（挂在闸门上），之后回一段文字。
    struct Routed {
        main: Mutex<Vec<Vec<Item>>>,
        child: Mutex<Vec<ModelRequest>>,
        gate: (watch::Sender<bool>, watch::Receiver<bool>),
    }

    impl Routed {
        fn new(main: Vec<Vec<Item>>) -> Arc<Self> {
            Arc::new(Self {
                main: Mutex::new(main.into_iter().rev().collect()),
                child: Mutex::default(),
                gate: watch::channel(false),
            })
        }

        fn child_requests(&self) -> Vec<ModelRequest> {
            self.child.lock().unwrap().clone()
        }
    }

    impl ModelPort for Routed {
        fn stream(
            &self,
            request: ModelRequest,
            cancel: CancellationToken,
        ) -> BoxStream<'static, Item> {
            let reply = match &request.source {
                QuerySource::MainThread => {
                    let next = self.main.lock().unwrap().pop();
                    stream::iter(next.unwrap_or_else(|| text_response(&["Noted."]))).boxed()
                }
                QuerySource::Subagent(_) => {
                    let first = self.child.lock().unwrap().is_empty();
                    self.child.lock().unwrap().push(request);
                    if first {
                        let events = tool_use_response(
                            "toolu_child_bash",
                            "Bash",
                            &json!({"command": "touch made-by-child.txt", "description": "create a file"}),
                        );
                        let mut open = self.gate.1.clone();
                        stream::once(async move {
                            let _ = open.wait_for(|open| *open).await;
                        })
                        .flat_map(move |()| stream::iter(events.clone()))
                        .boxed()
                    } else {
                        stream::iter(text_response(&["Child done."])).boxed()
                    }
                }
                _ => stream::iter(text_response(&["Side reply"])).boxed(),
            };
            reply.take_until(cancel.cancelled_owned()).boxed()
        }
    }

    fn spawn() -> Vec<Item> {
        tool_use_response(
            "toolu_main_agent",
            "Agent",
            &json!({"description": "Make the file", "prompt": "Create the file.",
                    "subagent_type": "general-purpose", "run_in_background": true}),
        )
    }

    fn workspace() -> WorkspaceRef {
        WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/Users/dev/work"),
        }
    }

    struct Run {
        _kernel: LocalKernel,
        session: Arc<dyn SessionHandle>,
        events: BoxStream<'static, SequencedEvent>,
        model: Arc<Routed>,
    }

    async fn start(surface: Surface) -> Run {
        let model = Routed::new(vec![spawn(), text_response(&["Launched."])]);
        let store = Arc::new(MemoryStore::default());
        let kernel = LocalKernel::with_context(
            Ports {
                model: model.clone(),
                environments: environments(Arc::new(UnusedEnvironment)),
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
            ContextConfig::new(surface),
        );
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
        let session = kernel.open_session(&id).await.unwrap();
        let events = session.subscribe(Some(0));
        submit(&session, "c1", "Make the file in the background.").await;
        Run {
            _kernel: kernel,
            session,
            events,
            model,
        }
    }

    async fn submit(session: &Arc<dyn SessionHandle>, id: &str, text: &str) {
        let ack = session
            .send(CommandEnvelope {
                id: CommandId::new(id),
                command: Command::Submit {
                    text: text.to_owned(),
                    attachments: Vec::new(),
                    delivery: Delivery::Queue,
                },
            })
            .await;
        assert!(matches!(ack, Ack::Accepted { .. }), "{ack:?}");
    }

    async fn send(session: &Arc<dyn SessionHandle>, id: &str, command: Command) -> Ack {
        session
            .send(CommandEnvelope {
                id: CommandId::new(id),
                command,
            })
            .await
    }

    /// 等到 `pick` 认出的事件（5 s 内）。
    async fn next<T>(
        events: &mut BoxStream<'static, SequencedEvent>,
        what: &str,
        pick: impl Fn(&KernelEvent) -> Option<T>,
    ) -> T {
        loop {
            let event = tokio::time::timeout(Duration::from_secs(5), events.next())
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
                .unwrap();
            if let Some(found) = pick(&event.event) {
                return found;
            }
        }
    }

    fn turn_finished(event: &KernelEvent) -> Option<()> {
        matches!(event, KernelEvent::TurnFinished { .. }).then_some(())
    }

    fn asked(event: &KernelEvent) -> Option<(InteractionId, InteractionRequest)> {
        match event {
            KernelEvent::InteractionAsked {
                interaction,
                request,
            } => Some((interaction.clone(), request.clone())),
            _ => None,
        }
    }

    /// 主回合结束之后才放行子代理的 Bash，等它的询问。
    async fn ask_after_the_turn(run: &mut Run) -> (InteractionId, InteractionRequest) {
        next(&mut run.events, "the main turn", turn_finished).await;
        run.model.gate.0.send_replace(true);
        next(&mut run.events, "the background ask", asked).await
    }

    fn last_result(request: &ModelRequest) -> (String, bool) {
        request
            .messages
            .iter()
            .flat_map(|message| message.content.iter())
            .filter_map(|content| match &content.block {
                ContentBlock::ToolResult {
                    content, is_error, ..
                } => {
                    let text = content
                        .iter()
                        .find_map(|part| match part {
                            ToolResultContent::Text { text } => Some(text.clone()),
                            _ => None,
                        })
                        .unwrap_or_default();
                    Some((text, *is_error))
                }
                _ => None,
            })
            .next_back()
            .unwrap()
    }

    async fn resolve(run: &Run, id: &str, interaction: InteractionId, answer: Value) {
        let ack = send(
            &run.session,
            id,
            Command::ResolveInteraction {
                interaction,
                answer,
            },
        )
        .await;
        assert!(matches!(ack, Ack::Accepted { .. }), "{ack:?}");
    }

    #[tokio::test]
    async fn a_background_ask_outlives_the_turn_and_names_the_agent() {
        let mut run = start(Surface::Interactive).await;
        let (interaction, request) = ask_after_the_turn(&mut run).await;
        let InteractionRequest::Permission { tool, asked_by, .. } = &request else {
            panic!("{request:?}");
        };
        assert_eq!(tool, "Bash");
        assert_eq!(asked_by.as_deref(), Some("general-purpose"));
        // 主线程再跑一个回合：回合结束只取消回合里的询问，后台子代理的询问留着。
        submit(&run.session, "c2", "Another question.").await;
        next(&mut run.events, "the second turn", turn_finished).await;
        let view = run.session.view().await.unwrap();
        assert!(
            view.pending.iter().any(|(id, _)| *id == interaction),
            "{view:?}"
        );
        let subagents = run.session.subagents().await.unwrap();
        assert!(
            subagents
                .iter()
                .any(|entry| entry.background && entry.waiting),
            "{subagents:?}"
        );
        resolve(&run, "answer", interaction, json!({"decision": "allow"})).await;
        // 允许之后工具执行了（执行环境是不能用的替身：结果是执行环境的错误，不是拒绝）。
        next(&mut run.events, "the notice turn", turn_finished).await;
        let child = run.model.child_requests();
        assert_eq!(child.len(), 2);
        let (text, _) = last_result(&child[1]);
        assert!(
            !text.contains("Permission for this tool use was denied"),
            "{text}"
        );
        assert!(!text.contains("requested permissions"), "{text}");
    }

    #[tokio::test]
    async fn interrupting_the_main_turn_keeps_the_background_ask() {
        let mut run = start(Surface::Interactive).await;
        let (interaction, _) = ask_after_the_turn(&mut run).await;
        // 主线程的回合在跑时按 esc：只取消回合里的询问。
        run.model.main.lock().unwrap().push(tool_use_response(
            "toolu_main_bash",
            "Bash",
            &json!({"command": "touch made-by-main.txt"}),
        ));
        submit(&run.session, "c2", "Make another file.").await;
        let (main_ask, _) = next(&mut run.events, "the main turn's ask", asked).await;
        let ack = send(&run.session, "esc", Command::Interrupt).await;
        assert!(matches!(ack, Ack::Accepted { .. }), "{ack:?}");
        next(&mut run.events, "the interrupted turn", turn_finished).await;
        let view = run.session.view().await.unwrap();
        let pending: Vec<_> = view.pending.iter().map(|(id, _)| id.clone()).collect();
        assert_eq!(pending, vec![interaction], "{main_ask:?}");
    }

    #[tokio::test]
    async fn a_denied_background_ask_gives_the_subagent_text() {
        let mut run = start(Surface::Interactive).await;
        let (interaction, _) = ask_after_the_turn(&mut run).await;
        resolve(&run, "answer", interaction, json!({"decision": "deny"})).await;
        next(&mut run.events, "the notice turn", turn_finished).await;
        let (text, is_error) = last_result(&run.model.child_requests()[1]);
        // 结果后面接着这一步的 attachment（agent 清单等），文字在最前面。
        assert!(text.starts_with(SUBAGENT_DENIED), "{text}");
        assert!(is_error);
    }

    #[tokio::test]
    async fn stopping_the_agent_withdraws_its_ask() {
        let mut run = start(Surface::Interactive).await;
        let (interaction, _) = ask_after_the_turn(&mut run).await;
        let agent = run.session.subagents().await.unwrap()[0].agent.clone();
        let ack = send(
            &run.session,
            "stop",
            Command::StopTask {
                task: agent.as_str().to_owned(),
            },
        )
        .await;
        assert!(matches!(ack, Ack::Accepted { .. }), "{ack:?}");
        let resolved = next(&mut run.events, "the withdrawn ask", |event| match event {
            KernelEvent::InteractionResolved { interaction } => Some(interaction.clone()),
            _ => None,
        })
        .await;
        assert_eq!(resolved, interaction);
        let view = run.session.view().await.unwrap();
        assert!(view.pending.is_empty(), "{view:?}");
    }

    #[tokio::test]
    async fn closing_the_session_cancels_the_background_ask() {
        let mut run = start(Surface::Interactive).await;
        let (interaction, _) = ask_after_the_turn(&mut run).await;
        // `/clear`：父会话关闭，待回答的交互全部取消；子代理按拒绝处理，照样跑完。
        run.session.close(EndReason::Clear).await;
        let resolved = next(&mut run.events, "the cancelled ask", |event| match event {
            KernelEvent::InteractionResolved { interaction } => Some(interaction.clone()),
            _ => None,
        })
        .await;
        assert_eq!(resolved, interaction);
        let model = run.model.clone();
        tokio::time::timeout(Duration::from_secs(5), async move {
            while model.child_requests().len() < 2 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the child continues");
        let (text, _) = last_result(&run.model.child_requests()[1]);
        assert!(text.starts_with(SUBAGENT_DENIED), "{text}");
    }

    #[tokio::test]
    async fn print_mode_background_agents_do_not_ask() {
        let mut run = start(Surface::Print).await;
        next(&mut run.events, "the main turn", turn_finished).await;
        run.model.gate.0.send_replace(true);
        // 子代理结束后的通知回合；其间没有询问。
        let asked_any = loop {
            let event = tokio::time::timeout(Duration::from_secs(5), run.events.next())
                .await
                .expect("the notice turn")
                .unwrap();
            match event.event {
                KernelEvent::InteractionAsked { .. } => break true,
                KernelEvent::TurnFinished { .. } => break false,
                _ => {}
            }
        };
        assert!(!asked_any);
        let (text, is_error) = last_result(&run.model.child_requests()[1]);
        // 没有询问的入口：PermissionRequest hooks 没有结论时拒绝，文字是“不能询问”的拒绝（工作线 F69），不是询问
        // 本身的文字，也不是用户拒绝的文字。
        assert!(is_error);
        assert!(
            text.starts_with(
                "Permission to use Bash has been denied. IMPORTANT: You *may* attempt to accomplish this action"
            ),
            "{text}"
        );
        // 之后可能接着当步的提醒（`<system-reminder>`），只看拒绝的文字本身。
        assert!(
            text.contains("STOP and explain to the user what you were trying to do and why you need this permission. Let the user decide how to proceed."),
            "{text}"
        );
    }
}
