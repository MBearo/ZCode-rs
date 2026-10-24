//! 整个工具的 deny 规则从工具清单里去掉（docs/specs/rust-cli/tools-files.md §1.1.1，工作线 F69）：LocalKernel + 真实
//! 主循环 + agents，看主线程与子代理的请求里的工具表。

// 集成测试里的辅助函数也按测试代码处理（clippy 的 allow-unwrap-in-tests 只认 cfg(test)）。
#[cfg(test)]
mod denied_tools {
    use std::sync::Arc;
    use std::time::Duration;

    use futures_util::StreamExt;
    use futures_util::stream::BoxStream;
    use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
    use rust_cli_kernel_api::ids::{CommandId, EnvironmentId};
    use rust_cli_kernel_api::model::{
        ModelError, ModelEvent, ModelPort, ModelRequest, QuerySource,
    };
    use rust_cli_kernel_api::permission::{
        PermissionMode, PermissionRules, RuleBehavior, RuleSource, SourcedRule,
    };
    use rust_cli_kernel_api::session_api::{
        Command, CommandEnvelope, CreateSession, Delivery, Kernel, KernelEvent, TurnOutcome,
    };
    use rust_cli_kernel_api::testing::{
        FixedClock, MemoryStore, ScriptedModel, SequentialIds, UnusedEnvironment, environments,
        selection, text_response, tool_use_response,
    };
    use rust_cli_policy::rules::parse_rule;
    use rust_cli_runtime::{Defaults, LocalKernel, Ports};
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    /// 主线程的请求按剧本，子代理的请求交给另一份剧本。
    struct Routed {
        main: ScriptedModel,
        subagent: ScriptedModel,
    }

    impl ModelPort for Routed {
        fn stream(
            &self,
            request: ModelRequest,
            cancel: CancellationToken,
        ) -> BoxStream<'static, Result<ModelEvent, ModelError>> {
            match &request.source {
                QuerySource::Subagent(_) => self.subagent.stream(request, cancel),
                _ => self.main.stream(request, cancel),
            }
        }
    }

    fn denied(texts: &[&str]) -> PermissionRules {
        PermissionRules {
            rules: texts
                .iter()
                .map(|text| SourcedRule {
                    source: RuleSource::CliArg,
                    behavior: RuleBehavior::Deny,
                    rule: parse_rule(text),
                })
                .collect(),
            ..PermissionRules::default()
        }
    }

    fn names(request: &ModelRequest) -> Vec<String> {
        request.tools.iter().map(|tool| tool.name.clone()).collect()
    }

    /// `--disallowedTools Bash,Edit,TaskStop` 的会话：主线程第一个请求里没有这三个工具，补回 Glob / Grep；前台子代理
    /// 的工具表从主线程的清单过滤，同样没有它们。
    #[tokio::test]
    async fn denied_tools_leave_the_main_and_subagent_tool_lists() {
        let model = Arc::new(Routed {
            main: ScriptedModel::new(vec![
                tool_use_response(
                    "toolu_1",
                    "Agent",
                    &json!({"description": "Say hello", "prompt": "Say hello.", "subagent_type": "general-purpose"}),
                ),
                text_response(&["Agent checks done."]),
            ]),
            subagent: ScriptedModel::new(vec![text_response(&["Hello from the subagent."])]),
        });
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
                mode: PermissionMode::DontAsk,
                max_turns: None,
                search_tools: false,
                task_tools: None,
                limits: Default::default(),
            },
        );
        kernel.replace_permission_rules(denied(&["Bash", "Edit", "TaskStop"]));
        let id = kernel
            .create_session(CreateSession {
                id: None,
                workspace: WorkspaceRef {
                    environment: EnvironmentId::new("local"),
                    root: EnvPath::new("/Users/dev/work"),
                },
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
        loop {
            let event = tokio::time::timeout(Duration::from_secs(5), events.next())
                .await
                .unwrap()
                .unwrap();
            if let KernelEvent::TurnFinished { outcome, .. } = event.event {
                assert_eq!(outcome, TurnOutcome::Success);
                break;
            }
        }
        let main = names(&model.main.requests()[0]);
        let child = names(&model.subagent.requests()[0]);
        for tools in [&main, &child] {
            for gone in ["Bash", "Edit", "TaskStop"] {
                assert!(!tools.iter().any(|name| name == gone), "{gone}: {tools:?}");
            }
            for back in ["Glob", "Grep", "Read", "Write"] {
                assert!(tools.iter().any(|name| name == back), "{back}: {tools:?}");
            }
        }
        assert!(main.iter().any(|name| name == "Agent"), "{main:?}");
    }
}
