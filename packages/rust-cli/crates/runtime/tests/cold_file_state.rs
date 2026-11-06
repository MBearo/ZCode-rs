//! 冷加载恢复“先读后改”记录（docs/specs/rust-cli/tools-files.md §4，architecture-review.md D4）：LocalKernel + 真实
//! 主循环与文件工具 + 内存执行环境。会话里 Read 过文件，关闭（释放 readFileState）后再打开（从存储冷加载），模型直接
//! Edit 同一个文件：照常询问权限、允许后写入，不再以 `File has not been read yet` 拒绝。
//!
//! 用 default 模式：编辑不经询问就允许时（acceptEdits、allow 规则）Edit 本来就不要求先读（tools-files.md §4）。

// 集成测试里的辅助函数也按测试代码处理（clippy 的 allow-unwrap-in-tests 只认 cfg(test)）。
#[cfg(test)]
mod cold_file_state {
    use std::sync::Arc;
    use std::time::Duration;

    use futures_util::StreamExt;
    use rust_cli_kernel_api::env::{EnvFacts, EnvPath, WorkspaceRef};
    use rust_cli_kernel_api::ids::{CommandId, EnvironmentId};
    use rust_cli_kernel_api::machine::Files;
    use rust_cli_kernel_api::message::{ContentBlock, Message};
    use rust_cli_kernel_api::permission::PermissionMode;
    use rust_cli_kernel_api::session_api::{
        Command, CommandEnvelope, CreateSession, Delivery, EndReason, Kernel, KernelEvent,
        SessionHandle,
    };
    use rust_cli_kernel_api::testing::{
        FixedClock, MemoryEnvironment, MemoryStore, ScriptedModel, SequentialIds, environments,
        selection, text_response, tool_use_response,
    };
    use rust_cli_runtime::{Defaults, LocalKernel, Ports};
    use serde_json::json;

    const WAIT: Duration = Duration::from_secs(5);
    const ROOT: &str = "/Users/dev/work";
    const FILE: &str = "/Users/dev/work/a.txt";

    fn environment() -> Arc<MemoryEnvironment> {
        Arc::new(
            MemoryEnvironment::new(EnvFacts {
                cwd: EnvPath::new(ROOT),
                platform: "darwin".to_owned(),
                os_version: String::new(),
                shell: "zsh".to_owned(),
                is_git_repo: false,
                home: Some(EnvPath::new("/Users/dev")),
                uid: None,
            })
            .with_file(FILE, "alpha\n"),
        )
    }

    fn workspace() -> WorkspaceRef {
        WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new(ROOT),
        }
    }

    /// 跑一个回合，期间的权限询问一律允许；返回询问的次数。
    async fn run_a_turn(session: &Arc<dyn SessionHandle>, id: &str) -> usize {
        let mut events = session.subscribe(Some(0));
        let mut asked = 0;
        session
            .send(CommandEnvelope {
                id: CommandId::new(id),
                command: Command::Submit {
                    text: "Change alpha to beta.".to_owned(),
                    attachments: Vec::new(),
                    delivery: Delivery::Queue,
                },
            })
            .await;
        loop {
            let event = tokio::time::timeout(WAIT, events.next())
                .await
                .expect("the turn must finish")
                .unwrap();
            match event.event {
                KernelEvent::InteractionAsked { interaction, .. } => {
                    asked += 1;
                    session
                        .send(CommandEnvelope {
                            id: CommandId::new(format!("{id}-allow-{asked}")),
                            command: Command::ResolveInteraction {
                                interaction,
                                answer:
                                    rust_cli_kernel_api::orchestration::PermissionAnswer::Allow {
                                        remember: Vec::new(),
                                    }
                                    .into(),
                            },
                        })
                        .await;
                }
                KernelEvent::TurnFinished { .. } => return asked,
                _ => {}
            }
        }
    }

    /// 这次 Edit 的工具结果（最后一条带 tool_result 的消息）。
    async fn edit_result(session: &Arc<dyn SessionHandle>) -> (bool, String) {
        let view = session.view().await.unwrap();
        view.messages
            .iter()
            .rev()
            .find_map(|message| match message {
                Message::User(user) => user.content.iter().find_map(|block| match block {
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } if tool_use_id.as_str() == "toolu_edit" => {
                        Some((*is_error, format!("{content:?}")))
                    }
                    _ => None,
                }),
                _ => None,
            })
            .expect("the Edit call must have a result")
    }

    #[tokio::test]
    async fn an_edit_after_a_cold_load_uses_the_reads_in_the_history() {
        let env = environment();
        let store = Arc::new(MemoryStore::default());
        let model = Arc::new(ScriptedModel::new(vec![
            tool_use_response("toolu_read", "Read", &json!({"file_path": FILE})),
            text_response(&["Read it."]),
            tool_use_response(
                "toolu_edit",
                "Edit",
                &json!({"file_path": FILE, "old_string": "alpha", "new_string": "beta"}),
            ),
            text_response(&["Changed it."]),
        ]));
        let kernel = LocalKernel::new(
            Ports {
                model: model.clone(),
                environments: environments(env.clone()),
                store: store.clone(),
                artifacts: store,
                clock: Arc::new(FixedClock(1_000)),
                ids: Arc::new(SequentialIds::default()),
            },
            Defaults {
                selection: selection().into(),
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
                workspace: workspace(),
                selection: selection(),
                mode: PermissionMode::Default,
                plan: false,
            })
            .await
            .unwrap();
        let first = kernel.open_session(&id).await.unwrap();
        assert_eq!(
            run_a_turn(&first, "c1").await,
            0,
            "a Read in the workspace does not ask"
        );
        // 关闭释放这个会话的 readFileState（session-lifecycle.md §3.3）；再打开是从存储冷加载。
        first.close(EndReason::Resume).await;
        drop(first);
        let reopened = kernel.open_session(&id).await.unwrap();
        let asked = run_a_turn(&reopened, "c2").await;
        let (is_error, content) = edit_result(&reopened).await;
        assert!(!is_error, "{content}");
        assert_eq!(asked, 1, "the Edit asks once and is allowed");
        let file = env
            .read_file(
                &workspace(),
                rust_cli_kernel_api::env::ReadFile {
                    path: EnvPath::new(FILE),
                },
            )
            .await
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&file.bytes), "beta\n");
    }
}
