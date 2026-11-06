//! guarded 的危险命令只批准这一次（docs/specs/rust-cli/shell-permissions.md §4.6.1）：经 LocalKernel 的真实组装（判决链、
//! 执行管线、hooks 引擎、会话 actor），PermissionRequest hook 回 allow 时仍然询问用户，hook 不运行。

// 集成测试里的辅助函数也按测试代码处理（clippy 的 allow-unwrap-in-tests 只认 cfg(test)）。
#[cfg(test)]
mod guarded_hooks {
    use rust_cli_base::sync::LockUnpoisoned;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use async_trait::async_trait;
    use futures_util::StreamExt;
    use futures_util::stream::BoxStream;
    use rust_cli_kernel_api::env::{
        CommandEvent, CommandKind, EnvError, EnvPath, OperationContext, Outcome, OutputStream,
        StartCommand, WorkspaceRef,
    };
    use rust_cli_kernel_api::ids::{CommandId, EnvironmentId, OperationId, SessionId};
    use rust_cli_kernel_api::machine::Commands;
    use rust_cli_kernel_api::message::{ContentBlock, ToolResultContent};
    use rust_cli_kernel_api::orchestration::InteractionRequest;
    use rust_cli_kernel_api::permission::PermissionMode;
    use rust_cli_kernel_api::session_api::{
        Ack, Command, CommandEnvelope, CreateSession, Delivery, Kernel, KernelEvent,
        SequencedEvent, SessionHandle,
    };
    use rust_cli_kernel_api::testing::{
        FixedClock, MemoryStore, ScriptedModel, SequentialIds, registry, selection, text_response,
        tool_use_response, unsupported_machine,
    };
    use rust_cli_runtime::{
        ContextConfig, Defaults, HookSource, HooksConfig, HooksSettings, LocalKernel, Ports,
        Surface,
    };
    use serde_json::json;

    /// guarded 的危险命令询问的文字（Node 原文）。
    const GUARDED_ASK: &str = "Dangerous command requires your approval for this execution";
    /// 命中 `remove-critical-path` 规则的命令（`rm` 带 `-r`）。
    const DANGEROUS: &str = "rm -rf build";

    /// 命令端口：hook 命令回 allow；别的命令（Bash 工具）只记下，立即结束。机器的其余端口返回 `Unsupported`。
    #[derive(Default)]
    struct HookEnv {
        hooks: Mutex<Vec<String>>,
        commands: Mutex<Vec<String>>,
    }

    fn unsupported() -> EnvError {
        EnvError::Unsupported {
            what: "test environment".to_owned(),
        }
    }

    fn finished() -> CommandEvent {
        CommandEvent::Finished {
            outcome: Outcome::Completed,
            exit_code: Some(0),
            timed_out: false,
            cwd_reset: None,
            output_bytes: 0,
            output_limit: None,
        }
    }

    #[async_trait]
    impl Commands for HookEnv {
        async fn start_command(
            &self,
            _workspace: &WorkspaceRef,
            _context: OperationContext,
            request: StartCommand,
        ) -> Result<BoxStream<'static, Result<CommandEvent, EnvError>>, EnvError> {
            let mut events = vec![CommandEvent::Started {
                output: EnvPath::new("/tmp/out"),
            }];
            if matches!(request.kind, CommandKind::Hook { .. }) {
                self.hooks.lock_unpoisoned().push(request.command);
                let allow = json!({"hookSpecificOutput": {
                    "hookEventName": "PermissionRequest",
                    "decision": {"behavior": "allow"},
                }});
                events.push(CommandEvent::Output {
                    stream: OutputStream::Stdout,
                    data: allow.to_string().into_bytes(),
                    cursor: 0,
                });
            } else {
                self.commands.lock_unpoisoned().push(request.command);
            }
            events.push(finished());
            Ok(futures_util::stream::iter(events.into_iter().map(Ok)).boxed())
        }

        fn watch_command(
            &self,
            _operation: OperationId,
            _cursor: u64,
        ) -> BoxStream<'static, Result<CommandEvent, EnvError>> {
            futures_util::stream::iter([Err(unsupported())]).boxed()
        }

        async fn cancel(&self, _operation: OperationId) -> Result<(), EnvError> {
            Ok(())
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

    /// 用户配置的 PermissionRequest hook：Bash 的询问一律 allow。
    fn allowing_hook() -> HooksConfig {
        let (settings, warnings) = HooksSettings::from_json(&json!({"hooks": {
            "PermissionRequest": [{
                "matcher": "Bash",
                "hooks": [{"type": "command", "command": "approve-everything"}],
            }],
        }}));
        assert!(warnings.is_empty(), "{warnings:?}");
        HooksConfig {
            sources: vec![(HookSource::User, settings)],
            ..HooksConfig::default()
        }
    }

    struct Run {
        model: Arc<ScriptedModel>,
        env: Arc<HookEnv>,
        session: Arc<dyn SessionHandle>,
        events: BoxStream<'static, SequencedEvent>,
        // 内核要活到测试结束。
        _kernel: LocalKernel,
    }

    /// guarded 会话里模型调用一次危险的 Bash 命令。
    async fn start(surface: Surface) -> Run {
        let model = Arc::new(ScriptedModel::new(vec![
            tool_use_response("toolu_rm", "Bash", &json!({"command": DANGEROUS})),
            text_response(&["done"]),
        ]));
        let env = Arc::new(HookEnv::default());
        let store = Arc::new(MemoryStore::default());
        let kernel = LocalKernel::with_context(
            Ports {
                model: model.clone(),
                environments: registry(unsupported_machine().with_commands(env.clone())),
                store: store.clone(),
                artifacts: store,
                clock: Arc::new(FixedClock(1_000)),
                ids: Arc::new(SequentialIds::default()),
            },
            Defaults {
                selection: selection().into(),
                mode: PermissionMode::Guarded,
                max_turns: None,
                search_tools: false,
                task_tools: None,
                limits: Default::default(),
            },
            ContextConfig::new(surface),
        );
        kernel.replace_hooks(allowing_hook());
        let id = kernel
            .create_session(CreateSession {
                id: None,
                workspace: workspace(),
                selection: selection(),
                mode: PermissionMode::Guarded,
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
                    text: "clean the build directory".to_owned(),
                    attachments: Vec::new(),
                    delivery: Delivery::Queue,
                },
            })
            .await;
        // 先到的是询问还是回合结束。
        let first = loop {
            let event = tokio::time::timeout(Duration::from_secs(10), events.next())
                .await
                .expect("the turn must ask or finish")
                .unwrap();
            if matches!(
                event.event,
                KernelEvent::InteractionAsked { .. } | KernelEvent::TurnFinished { .. }
            ) {
                break event.event;
            }
        };
        let KernelEvent::InteractionAsked {
            interaction,
            request:
                InteractionRequest::Permission {
                    message,
                    suggestions,
                    ..
                },
        } = first
        else {
            panic!(
                "the dangerous command must ask the user: {first:?}; hooks run: {:?}, commands run: {:?}",
                env.hooks.lock_unpoisoned(),
                env.commands.lock_unpoisoned(),
            );
        };
        assert_eq!(message, GUARDED_ASK);
        // 只批准这一次：没有“总是允许”。
        assert!(suggestions.is_empty(), "{suggestions:?}");
        let answer = session
            .send(CommandEnvelope {
                id: CommandId::new("c2"),
                command: Command::ResolveInteraction {
                    interaction,
                    answer: match surface {
                        Surface::Interactive => {
                            rust_cli_kernel_api::orchestration::PermissionAnswer::Deny {
                                feedback: None,
                            }
                            .into()
                        }
                        // `-p` 没有人回答（headless 的驱动答 Unavailable）。
                        Surface::Print => {
                            rust_cli_kernel_api::orchestration::PermissionAnswer::Unavailable.into()
                        }
                    },
                },
            })
            .await;
        assert!(matches!(answer, Ack::Accepted { .. }), "{answer:?}");
        Run {
            model,
            env,
            session,
            events,
            _kernel: kernel,
        }
    }

    async fn finish(run: &mut Run) {
        loop {
            let event = tokio::time::timeout(Duration::from_secs(10), run.events.next())
                .await
                .expect("the turn must finish")
                .unwrap();
            if matches!(event.event, KernelEvent::TurnFinished { .. }) {
                return;
            }
        }
    }

    /// 第二个请求里 `toolu_rm` 的结果：（文字，是不是错误）。
    fn rm_result(model: &ScriptedModel) -> (String, bool) {
        let requests = model.requests();
        requests[1]
            .messages
            .iter()
            .flat_map(|message| message.content.iter())
            .find_map(|content| match &content.block {
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } if tool_use_id.as_str() == "toolu_rm" => {
                    let text = content
                        .iter()
                        .filter_map(|part| match part {
                            ToolResultContent::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<String>();
                    Some((text, *is_error))
                }
                _ => None,
            })
            .expect("the second request carries the rm result")
    }

    fn assert_nothing_ran(env: &HookEnv) {
        let hooks: Vec<String> = env.hooks.lock_unpoisoned().clone();
        let commands: Vec<String> = env.commands.lock_unpoisoned().clone();
        assert!(
            hooks.is_empty(),
            "PermissionRequest hooks must not run: {hooks:?}"
        );
        assert!(
            commands.is_empty(),
            "the command must not run: {commands:?}"
        );
    }

    /// 缺陷 D1（architecture-review.md §1）：以前执行管线拿到的判决链是 runtime 的包装层，它没有转发“只批准这一次”，
    /// 管线照常跑 PermissionRequest hook，hook 回 allow 时命令直接执行，用户没有看到询问。
    #[tokio::test(flavor = "multi_thread")]
    async fn a_permission_request_hook_cannot_approve_a_guarded_dangerous_command() {
        let mut run = start(Surface::Interactive).await;
        finish(&mut run).await;
        assert_nothing_ran(&run.env);
        let (text, is_error) = rm_result(&run.model);
        assert!(is_error, "{text}");
        assert!(run.session.view().await.unwrap().pending.is_empty());
    }

    /// `-p` 没有人回答：模型收到询问本身的文字（shell-permissions.md §4.6.1、§4.8），hook 仍然不运行。
    #[tokio::test(flavor = "multi_thread")]
    async fn print_mode_returns_the_guarded_question_as_the_result() {
        let mut run = start(Surface::Print).await;
        finish(&mut run).await;
        assert_nothing_ran(&run.env);
        let (text, is_error) = rm_result(&run.model);
        assert!(is_error);
        assert!(text.starts_with(GUARDED_ASK), "{text}");
    }
}
