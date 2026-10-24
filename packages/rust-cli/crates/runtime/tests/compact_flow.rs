//! `/compact` 端到端（docs/specs/rust-cli/compact.md §3.2、§4.2）：真实的 LocalKernel、PromptAssembler、
//! QueryLoop 与 Compaction，模型用 ScriptedModel。检查摘要请求与主线程共享 system 和工具清单、
//! 来源是 compact、最后一条是摘要提示；压缩之后的请求只含摘要与新输入。
// 集成测试里的辅助函数也按测试代码处理（clippy 的 allow-unwrap-in-tests 只认 cfg(test)）。
#[cfg(test)]
mod flow {
    use std::sync::Arc;
    use std::time::Duration;

    use futures_util::StreamExt;
    use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
    use rust_cli_kernel_api::ids::{CommandId, EnvironmentId};
    use rust_cli_kernel_api::message::ContentBlock;
    use rust_cli_kernel_api::model::{ModelRequest, QuerySource};
    use rust_cli_kernel_api::permission::PermissionMode;
    use rust_cli_kernel_api::session_api::{
        Command, CommandEnvelope, CreateSession, Delivery, Kernel, KernelEvent, SessionHandle,
        TurnOutcome,
    };
    use rust_cli_kernel_api::testing::{
        FixedClock, MemoryStore, ScriptedModel, SequentialIds, UnusedEnvironment, environments,
        selection, text_response,
    };
    use rust_cli_runtime::{Defaults, LocalKernel, Ports};

    fn texts(request: &ModelRequest) -> Vec<String> {
        request
            .messages
            .iter()
            .flat_map(|message| message.content.iter())
            .filter_map(|item| match &item.block {
                ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    async fn finish(session: &Arc<dyn SessionHandle>, after: u64) -> (TurnOutcome, u64) {
        let mut events = session.subscribe(Some(after));
        loop {
            let event = tokio::time::timeout(Duration::from_secs(5), events.next())
                .await
                .unwrap()
                .unwrap();
            if let KernelEvent::TurnFinished { outcome, .. } = event.event {
                return (outcome, event.seq);
            }
        }
    }

    fn command(id: &str, command: Command) -> CommandEnvelope {
        CommandEnvelope {
            id: CommandId::new(id),
            command,
        }
    }

    fn submit(text: &str) -> Command {
        Command::Submit {
            text: text.to_owned(),
            attachments: Vec::new(),
            delivery: Delivery::Queue,
        }
    }

    #[tokio::test]
    async fn compact_summarizes_with_the_main_prefix_and_later_requests_start_from_the_summary() {
        let model = Arc::new(ScriptedModel::new(vec![
            text_response(&["hi there"]),
            text_response(&["<analysis>a</analysis><summary>The user said hello.</summary>"]),
            text_response(&["ok"]),
        ]));
        let store = Arc::new(MemoryStore::default());
        let kernel = LocalKernel::new(
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
        );
        let id = kernel
            .create_session(CreateSession {
                id: None,
                workspace: WorkspaceRef {
                    environment: EnvironmentId::new("local"),
                    root: EnvPath::new("/Users/dev/project"),
                },
                selection: selection(),
                mode: PermissionMode::Default,
                plan: false,
            })
            .await
            .unwrap();
        let session = kernel.open_session(&id).await.unwrap();
        session.send(command("c1", submit("hello"))).await;
        let (outcome, seq) = finish(&session, 0).await;
        assert_eq!(outcome, TurnOutcome::Success);
        session
            .send(command("c2", Command::Compact { instructions: None }))
            .await;
        let (outcome, seq) = finish(&session, seq).await;
        assert_eq!(outcome, TurnOutcome::Success);
        session.send(command("c3", submit("next"))).await;
        let (outcome, _) = finish(&session, seq).await;
        assert_eq!(outcome, TurnOutcome::Success);

        let sent = model.requests();
        assert_eq!(sent.len(), 3);
        let (main, summary, after) = (&sent[0], &sent[1], &sent[2]);
        assert_eq!(summary.source, QuerySource::Compact);
        assert_eq!(summary.system, main.system);
        assert_eq!(summary.tools, main.tools);
        assert_eq!(summary.max_tokens, main.max_tokens);
        let summary_texts = texts(summary);
        assert!(summary_texts.iter().any(|text| text.contains("hello")));
        assert!(
            summary_texts
                .last()
                .unwrap()
                .starts_with("CRITICAL: Respond with TEXT ONLY.")
        );
        let after_texts = texts(after);
        // 压缩后重新宣布的 attachment 按 normalize 的上浮规则排在摘要之前（context.md §3）。
        let summary_text = after_texts
            .iter()
            .find(|text| {
                text.starts_with(
                    "This session is being continued from a previous conversation that ran out of context.",
                )
            })
            .unwrap_or_else(|| panic!("{after_texts:?}"));
        assert!(summary_text.contains("Summary:\nThe user said hello.\nContinue the conversation"));
        assert!(!after_texts.iter().any(|text| text == "hello"));
        assert!(after_texts.iter().any(|text| text.contains("next")));
    }
}
