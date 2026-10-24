//! 子代理端到端（docs/specs/rust-cli/agents.md）：LocalKernel + 真实主循环 + agents，模型按请求来源
//! 分流（主线程按剧本，子代理固定回一段报告）。

// 集成测试里的辅助函数也按测试代码处理（clippy 的 allow-unwrap-in-tests 只认 cfg(test)）。
#[cfg(test)]
mod agents {
    use std::sync::Arc;
    use std::time::Duration;

    use futures_util::StreamExt;
    use futures_util::stream::BoxStream;
    use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
    use rust_cli_kernel_api::ids::{CommandId, EnvironmentId, SessionId};
    use rust_cli_kernel_api::message::{ContentBlock, Message, ToolResultContent};
    use rust_cli_kernel_api::model::{
        ModelError, ModelEvent, ModelPort, ModelRequest, QuerySource,
    };
    use rust_cli_kernel_api::permission::PermissionMode;
    use rust_cli_kernel_api::session_api::{
        Command, CommandEnvelope, CreateSession, Delivery, Kernel, KernelEvent, SessionHandle,
        TurnOutcome,
    };
    use rust_cli_kernel_api::store::SessionStore;
    use rust_cli_kernel_api::testing::{
        FixedClock, MemoryStore, ScriptedModel, SequentialIds, UnusedEnvironment, environments,
        selection, text_response, tool_use_response,
    };
    use rust_cli_runtime::{Defaults, LocalKernel, Ports};
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    /// 主线程的请求按剧本，子代理的请求交给另一份剧本。`hold` 有值时，子代理的回复等主线程发出这么
    /// 多个请求之后才给出（让后台子代理在主线程的最后一个工具边界之后结束，L18）。
    struct Routed {
        main: ScriptedModel,
        subagent: ScriptedModel,
        hold: Option<(usize, Arc<tokio::sync::Notify>)>,
    }

    impl ModelPort for Routed {
        fn stream(
            &self,
            request: ModelRequest,
            cancel: CancellationToken,
        ) -> BoxStream<'static, Result<ModelEvent, ModelError>> {
            match (&request.source, &self.hold) {
                (QuerySource::Subagent(_), None) => self.subagent.stream(request, cancel),
                (QuerySource::Subagent(_), Some((_, gate))) => {
                    let gate = gate.clone();
                    let reply = self.subagent.stream(request, cancel);
                    futures_util::stream::once(async move {
                        gate.notified().await;
                        reply
                    })
                    .flatten()
                    .boxed()
                }
                _ => {
                    let stream = self.main.stream(request, cancel);
                    if let Some((count, gate)) = &self.hold
                        && self.main.requests().len() == *count
                    {
                        gate.notify_one();
                    }
                    stream
                }
            }
        }
    }

    fn workspace() -> WorkspaceRef {
        WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/Users/dev/work"),
        }
    }

    async fn run(
        model: Arc<Routed>,
        store: Arc<MemoryStore>,
        turns: usize,
    ) -> (LocalKernel, Arc<dyn SessionHandle>) {
        let kernel = LocalKernel::new(
            Ports {
                model,
                environments: environments(Arc::new(UnusedEnvironment)),
                store: store.clone(),
                artifacts: store,
                clock: Arc::new(FixedClock(1_000)),
                ids: Arc::new(SequentialIds::default()),
            },
            Defaults {
                selection: selection(),
                mode: PermissionMode::DontAsk,
                max_turns: None,
                search_tools: false,
                task_tools: None,
                limits: Default::default(),
            },
        );
        let id = kernel
            .create_session(CreateSession {
                id: None,
                workspace: workspace(),
                selection: selection(),
                mode: PermissionMode::DontAsk,
                plan: false,
            })
            .await
            .unwrap();
        let session = kernel.open_session(&id).await.unwrap();
        let mut events = session.subscribe(Some(0));
        session
            .send(CommandEnvelope {
                id: CommandId::new("c1"),
                command: Command::Submit {
                    text: "Run the scripted agent checks.".to_owned(),
                    attachments: Vec::new(),
                    delivery: Delivery::Queue,
                },
            })
            .await;
        let mut finished = 0;
        while finished < turns {
            let event = tokio::time::timeout(Duration::from_secs(5), events.next())
                .await
                .unwrap()
                .unwrap();
            if let KernelEvent::TurnFinished { outcome, .. } = event.event {
                assert_eq!(outcome, TurnOutcome::Success);
                finished += 1;
            }
        }
        // kernel 释放时会话随之结束：交给调用方持有。
        (kernel, session)
    }

    fn tool_result_text(request: &ModelRequest) -> String {
        request
            .messages
            .iter()
            .flat_map(|message| message.content.iter())
            .find_map(|content| match &content.block {
                ContentBlock::ToolResult { content, .. } => {
                    content.iter().find_map(|part| match part {
                        ToolResultContent::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                }
                _ => None,
            })
            .unwrap()
    }

    #[tokio::test]
    async fn a_foreground_agent_runs_a_child_loop_and_hands_back_its_report() {
        let model = Arc::new(Routed {
            main: ScriptedModel::new(vec![
                tool_use_response(
                    "toolu_1",
                    "Agent",
                    &json!({"description": "Say hello", "prompt": "Say hello from the subagent.", "subagent_type": "general-purpose", "run_in_background": false}),
                ),
                text_response(&["Agent checks done."]),
            ]),
            subagent: ScriptedModel::new(vec![text_response(&["Hello from the subagent."])]),
            hold: None,
        });
        let store = Arc::new(MemoryStore::default());
        let (_kernel, session) = run(model.clone(), store.clone(), 1).await;
        let child = &model.subagent.requests()[0];
        assert_eq!(
            child.system[0].text,
            "You are ZCode, an interactive coding agent."
        );
        assert!(
            child.system[1]
                .text
                .starts_with("You are an agent for ZCode. Given the user's message")
        );
        assert!(
            child.system[1]
                .text
                .ends_with("<total_tokens>15000000 tokens left</total_tokens>")
        );
        let tools: Vec<&str> = child.tools.iter().map(|tool| tool.name.as_str()).collect();
        assert!(
            tools.contains(&"Agent") && tools.contains(&"Bash"),
            "{tools:?}"
        );
        let main = model.main.requests();
        assert!(main[0].tools.iter().any(|tool| tool.name == "Agent"));
        let handback = tool_result_text(&main[1]);
        assert!(handback.starts_with("[Subagent hand-back]"), "{handback}");
        assert!(
            handback.contains("\n  Hello from the subagent.\nagentId: agent_"),
            "{handback}"
        );
        // 子会话落库：parent_id 指向父会话，里面有 prompt 与子代理的回复。
        let children: Vec<SessionId> = store
            .list(rust_cli_kernel_api::store::SessionFilter {
                workspace: workspace(),
                limit: 10,
            })
            .await
            .unwrap()
            .into_iter()
            .map(|summary| summary.id)
            .filter(|id| id.as_str().starts_with("sess_subagent_"))
            .collect();
        assert_eq!(children.len(), 1);
        // 调用关联到子会话，与工具结果同批落库（修复线 F2：Node 的子代理查询靠它）。
        let links: Vec<_> = store
            .facts(session.id())
            .into_iter()
            .filter_map(|fact| match fact {
                rust_cli_kernel_api::store::Fact::SubagentLinked {
                    tool_use,
                    child,
                    agent_type,
                    ..
                } => Some((tool_use.as_str().to_owned(), child, agent_type)),
                _ => None,
            })
            .collect();
        assert_eq!(
            links,
            vec![(
                "toolu_1".to_owned(),
                children[0].clone(),
                "general-purpose".to_owned()
            )]
        );
        let loaded = store.load(&children[0]).await.unwrap();
        assert!(loaded.meta.parent.is_some());
        assert!(
            loaded
                .messages
                .iter()
                .any(|message| matches!(message, Message::Assistant(_)))
        );
    }

    /// 修复线 F51 / F51b（用户决定 2026-10-22，agents.md §2）：发给模型的 Agent 定义没有 `model` 与 `isolation`
    /// 参数；模型仍传了时按对象 schema 丢掉多出的键、不报错，子代理用父会话的模型、在父会话的工作区里跑。
    #[tokio::test]
    async fn the_agent_definition_has_no_model_or_isolation_and_passed_ones_are_dropped() {
        let model = Arc::new(Routed {
            main: ScriptedModel::new(vec![
                tool_use_response(
                    "toolu_1",
                    "Agent",
                    &json!({"description": "Say hello", "prompt": "Say hello from the subagent.", "model": "sonnet", "isolation": "worktree", "run_in_background": false}),
                ),
                text_response(&["Agent checks done."]),
            ]),
            subagent: ScriptedModel::new(vec![text_response(&["Hello from the subagent."])]),
            hold: None,
        });
        let store = Arc::new(MemoryStore::default());
        let (_kernel, _session) = run(model.clone(), store, 1).await;
        let main = model.main.requests();
        let agent = main[0]
            .tools
            .iter()
            .find(|tool| tool.name == "Agent")
            .unwrap();
        assert!(
            agent.input_schema["properties"].get("model").is_none(),
            "{}",
            agent.input_schema
        );
        assert!(
            agent.input_schema["properties"].get("isolation").is_none(),
            "{}",
            agent.input_schema
        );
        let handback = tool_result_text(&main[1]);
        assert!(handback.starts_with("[Subagent hand-back]"), "{handback}");
        let child = &model.subagent.requests()[0];
        assert_eq!(child.selection.model, selection().model);
    }

    #[tokio::test]
    async fn a_background_agent_launches_and_its_notice_opens_a_new_turn() {
        let model = Arc::new(Routed {
            main: ScriptedModel::new(vec![
                tool_use_response(
                    "toolu_1",
                    "Agent",
                    &json!({"description": "Find files", "prompt": "List the files.", "subagent_type": "Explore"}),
                ),
                text_response(&["Launched."]),
                text_response(&["Notified."]),
            ]),
            subagent: ScriptedModel::new(vec![text_response(&["Hello from the subagent."])]),
            // 子代理在主线程第二个请求（没有工具边界）之后才结束：通知开新回合。
            hold: Some((2, Arc::new(tokio::sync::Notify::new()))),
        });
        let store = Arc::new(MemoryStore::default());
        let (_kernel, session) = run(model.clone(), store.clone(), 2).await;
        let main = model.main.requests();
        assert!(tool_result_text(&main[1]).starts_with("Async agent launched successfully."));
        // 一次性子代理（Explore）的结果没有 agentId 尾注：关联照样落库（修复线 F2）。
        assert!(store.facts(session.id()).iter().any(|fact| matches!(
            fact,
            rust_cli_kernel_api::store::Fact::SubagentLinked { agent_type, .. } if agent_type == "Explore"
        )));
        let last = format!("{:?}", main[2].messages.last().unwrap());
        assert!(last.contains("<task-notification>"), "{last}");
        assert!(
            last.contains("<result>Hello from the subagent.</result>"),
            "{last}"
        );
        let view = session.view().await.unwrap();
        assert!(!view.running && view.background.is_empty());
    }
}
