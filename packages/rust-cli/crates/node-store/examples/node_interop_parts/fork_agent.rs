//! 互操作的第二十七段：fork 子代理的子会话（修复线 L-A，docs/specs/rust-cli/node-store.md §4.8.1、agents.md §8.1.3）。
//!
//! `fork-agent-write <storage-dir>`：父会话一轮输入与一条 fork 的 Agent 调用（结果是“已启动”、关联到子会话）；子会话照
//! agents 的写法：模型、模式、`rust-cli/fork_parent`、directive（第一条输入）、子代理的回答。继承的前缀不写进子会话。
//! 输出两个会话的 id 与 directive。
use super::*;
use rust_cli_base::models::GLM_5_3;

pub(super) async fn write(dir: &str) -> Result<()> {
    let store = store(dir)?;
    let parent = create(&store).await?;
    let (u1, a1) = (message_id(), message_id());
    let input = json!({"description": "Read the file", "prompt": "Read a.txt and report its first line.",
        "subagent_type": "fork"});
    store
        .commit(
            &parent,
            batch(
                Some("cmd_f1"),
                vec![
                    user(&u1, "Fork a worker to read a.txt."),
                    reply(
                        &a1,
                        vec![call("toolu_fork", "Agent", &input)],
                        StopReason::ToolUse,
                    ),
                ],
            ),
        )
        .await?;
    let agent = ids().subagent().0.to_string();
    let child = SessionId::new(format!("sess_subagent_{agent}"));
    store
        .create(SessionMeta {
            id: child.clone(),
            kind: SessionKind::Subagent,
            parent: Some(parent.clone()),
            workspace: WorkspaceRef {
                environment: EnvironmentId::new(LOCAL_ENVIRONMENT),
                root: EnvPath::new(WORKSPACE),
            },
            title: "Read the file".into(),
            trace: TraceId::new(parent.as_str()),
            created_at_ms: now_ms(),
        })
        .await?;
    // agents 写的 directive：固定段落之后是 `Your directive: <prompt>`（这里只要形状，段落取短的一段）。
    let directive = "<fork-boilerplate>\nYou are a worker fork.\n</fork-boilerplate>\n\nYour directive: Read a.txt and report its first line.";
    let (d1, d2) = (message_id(), message_id());
    let mut current = None;
    store
        .commit(
            &child,
            anchored(
                &mut current,
                vec![
                    Fact::ModelSelected {
                        selection: ModelSelection {
                            provider: "anthropic".into(),
                            model: GLM_5_3.into(),
                            reasoning: None,
                            connection: None,
                        },
                    },
                    Fact::ModeChanged {
                        mode: PermissionMode::Default,
                    },
                    Fact::Entry {
                        kind: "fork_parent".into(),
                        id: agent.clone(),
                        data: json!({"parentSession": parent.as_str(),
                            "throughMessageId": u1.as_str(), "toolUseId": "toolu_fork"}),
                    },
                    user(&d1, directive),
                ],
            ),
        )
        .await?;
    store
        .commit(
            &child,
            anchored(
                &mut current,
                vec![reply(
                    &d2,
                    vec![text("Fork report: alpha")],
                    StopReason::EndTurn,
                )],
            ),
        )
        .await?;
    let launched = "Async agent launched successfully.";
    let facts = vec![
        result(&a1, "toolu_fork", vec![said(launched)], false),
        Fact::SubagentLinked {
            tool_use: ToolUseId::new("toolu_fork"),
            agent: rust_cli_kernel_api::ids::AgentId::new(agent.as_str()),
            child: child.clone(),
            agent_type: "fork".into(),
        },
    ];
    store.commit(&parent, batch(None, facts)).await?;
    print(&json!({"parent": parent.as_str(), "child": child.as_str(), "directive": directive}))
}
