use std::sync::Arc;

use futures_util::StreamExt;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, SessionId};
use rust_cli_kernel_api::message::{ContentBlock, Message, UserOrigin};
use rust_cli_kernel_api::model::ModelError;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::store::SessionFilter;
use rust_cli_kernel_api::testing::{
    FixedClock, MemoryStore, NoticeEnvironment, ScriptedModel, SequentialIds, UnusedEnvironment,
    environments, selection, shell_notice, text_response, tool_use_response,
};
use rust_cli_runtime::{Defaults, LocalKernel, Ports};
use tokio_util::sync::CancellationToken;

use super::*;

fn kernel(model: ScriptedModel) -> Arc<dyn Kernel> {
    let store = Arc::new(MemoryStore::default());
    Arc::new(LocalKernel::new(
        Ports {
            model: Arc::new(model),
            environments: environments(Arc::new(UnusedEnvironment)),
            store: store.clone(),
            artifacts: store,
            clock: Arc::new(FixedClock(0)),
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
    ))
}

fn options() -> PrintOptions {
    PrintOptions {
        session_id: None,
        prompt: "hi".to_owned(),
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/work"),
        },
        selection: selection(),
        mode: PermissionMode::Default,
        target: SessionTarget::New,
        fork: false,
        stop: CancellationToken::new(),
        output: Output::default(),
        cancel: CancellationToken::new(),
    }
}

#[tokio::test]
async fn print_mode_writes_the_final_reply() {
    let mut output = Vec::new();
    run(
        kernel(ScriptedModel::new(vec![text_response(&["hel", "lo"])])),
        options(),
        &mut output,
    )
    .await
    .unwrap();
    assert_eq!(String::from_utf8(output).unwrap(), "hello\n");
}

#[tokio::test]
async fn a_model_failure_is_an_error() {
    let mut output = Vec::new();
    let error = run(
        kernel(ScriptedModel::new(vec![vec![Err(
            ModelError::Overloaded {
                message: "busy".to_owned(),
            },
        )]])),
        options(),
        &mut output,
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("busy"), "{error}");
    // F10：print 模式把 API 错误的文字写到 stdout（最后一个 result），退出码 1。
    let written = String::from_utf8(output).unwrap();
    assert!(
        written.ends_with('\n') && written.contains("busy"),
        "{written:?}"
    );
}

/// F10：轮数用尽时 stdout 是 `Error: Reached max turns (N)`（没有换行），退出码 1。
#[tokio::test]
async fn max_turns_writes_the_error_line() {
    let store = Arc::new(MemoryStore::default());
    let kernel = Arc::new(LocalKernel::new(
        Ports {
            model: Arc::new(ScriptedModel::new(vec![tool_use_response(
                "toolu_1",
                "Read",
                &serde_json::json!({"file_path": "a"}),
            )])),
            environments: environments(Arc::new(UnusedEnvironment)),
            store: store.clone(),
            artifacts: store,
            clock: Arc::new(FixedClock(0)),
            ids: Arc::new(SequentialIds::default()),
        },
        Defaults {
            selection: selection(),
            mode: PermissionMode::Default,
            max_turns: Some(1),
            search_tools: false,
            task_tools: None,
            limits: Default::default(),
        },
    ));
    let mut output = Vec::new();
    run(kernel, options(), &mut output).await.unwrap_err();
    assert_eq!(
        String::from_utf8(output).unwrap(),
        "Error: Reached max turns (1)"
    );
}

/// `-p` 等后台命令结束、通知处理完才收尾，输出最后一个回合的回复（query.md §5）。命令在
/// 第一次检查之后才结束：通知还在路上时也不能提前收尾。
#[tokio::test]
async fn print_mode_waits_for_background_commands_and_their_notices() {
    let env = Arc::new(NoticeEnvironment::default());
    // 查询依次是：提交前 headless 取会话位置、第一步组装上下文（环境段的 shell cwd）、第一个回合结束后
    // headless 看后台工作。命令在第三次之后才结束：通知还在路上时 headless 也不能收尾。
    env.finish_after_checks(shell_notice(1, "b1"), 3);
    let store = Arc::new(MemoryStore::default());
    let kernel = Arc::new(LocalKernel::new(
        Ports {
            model: Arc::new(ScriptedModel::new(vec![
                text_response(&["started"]),
                text_response(&["b1 is done"]),
            ])),
            environments: environments(env.clone()),
            store: store.clone(),
            artifacts: store,
            clock: Arc::new(FixedClock(0)),
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
    ));
    let mut output = Vec::new();
    run(kernel, options(), &mut output).await.unwrap();
    assert_eq!(String::from_utf8(output).unwrap(), "b1 is done\n");
    assert_eq!(env.acked(), [1]);
}

/// `-p` 没有人回答询问：答 `Unavailable`，模型收到询问本身的文本（print 模式）。
#[tokio::test]
async fn print_mode_answers_permission_asks_as_unavailable() {
    let model = ScriptedModel::new(vec![
        tool_use_response(
            "toolu_1",
            "Bash",
            &serde_json::json!({"command": "touch x"}),
        ),
        text_response(&["could not run it"]),
    ]);
    let model = Arc::new(model);
    let store = Arc::new(MemoryStore::default());
    let kernel = Arc::new(LocalKernel::new(
        Ports {
            model: model.clone(),
            environments: environments(Arc::new(UnusedEnvironment)),
            store: store.clone(),
            artifacts: store,
            clock: Arc::new(FixedClock(0)),
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
    ));
    let mut output = Vec::new();
    run(kernel, options(), &mut output).await.unwrap();
    assert_eq!(String::from_utf8(output).unwrap(), "could not run it\n");
    let result = format!("{:?}", model.requests()[1].messages);
    assert!(result.contains("is_error: true"), "{result}");
    assert!(result.contains("needs approval"), "{result}");
}

#[tokio::test]
async fn continue_appends_to_the_latest_session_and_resume_needs_a_known_id() {
    let kernel = kernel(ScriptedModel::new(vec![
        text_response(&["first"]),
        text_response(&["second"]),
    ]));
    let mut output = Vec::new();
    run(kernel.clone(), options(), &mut output).await.unwrap();
    let mut continued = options();
    continued.target = SessionTarget::Continue;
    let mut output = Vec::new();
    run(kernel.clone(), continued, &mut output).await.unwrap();
    assert_eq!(String::from_utf8(output).unwrap(), "second\n");
    // 两次提问在同一个会话里。
    let sessions = kernel
        .list_sessions(SessionFilter {
            workspace: options().workspace,
            limit: 10,
        })
        .await
        .unwrap();
    assert_eq!(sessions.len(), 1);
    let view = kernel
        .open_session(&sessions[0].id)
        .await
        .unwrap()
        .view()
        .await
        .unwrap();
    let humans = view
        .messages
        .iter()
        .filter(
            |message| matches!(message, Message::User(user) if user.origin == UserOrigin::Human),
        )
        .count();
    assert_eq!(humans, 2);

    let mut unknown = options();
    unknown.target = SessionTarget::Resume(SessionId::new("sess_missing"));
    let error = run(kernel, unknown, &mut Vec::new()).await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "No conversation found with session ID: sess_missing"
    );
}

/// `-p -c` 找不到可续写的会话时不报错，按新会话照常回答（session-lifecycle.md §1；交互界面仍报错）。
/// `--fork-session` 同样：没有可 fork 的会话，直接用新会话。
#[tokio::test]
async fn continue_without_a_session_starts_a_new_one() {
    for fork in [false, true] {
        let kernel = kernel(ScriptedModel::new(vec![text_response(&["fresh"])]));
        let mut continued = options();
        continued.target = SessionTarget::Continue;
        continued.fork = fork;
        let mut output = Vec::new();
        run(kernel.clone(), continued, &mut output).await.unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "fresh\n", "fork {fork}");
        let sessions = kernel
            .list_sessions(SessionFilter {
                workspace: options().workspace,
                limit: 10,
            })
            .await
            .unwrap();
        assert_eq!(sessions.len(), 1, "fork {fork}");
    }
}

/// stream-json 输入下 `-c` 找不到会话时同样用新会话。
#[tokio::test]
async fn continue_without_a_session_starts_a_new_one_for_stream_input() {
    let kernel = kernel(ScriptedModel::new(vec![text_response(&["fresh"])]));
    let mut continued = options();
    continued.target = SessionTarget::Continue;
    let input = "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n";
    let mut output = Vec::new();
    run_stream(kernel.clone(), continued, input.as_bytes(), &mut output)
        .await
        .unwrap();
    assert_eq!(String::from_utf8(output).unwrap(), "fresh\n");
}

/// `/usage` 与 `/context` 在 `-c` 找不到会话时与新会话相同：不建会话行。
#[tokio::test]
async fn local_commands_with_continue_and_no_session_create_nothing() {
    for prompt in ["/usage", "/context"] {
        let kernel = kernel(ScriptedModel::new(Vec::new()));
        let mut continued = options();
        continued.target = SessionTarget::Continue;
        continued.prompt = prompt.to_owned();
        run(kernel.clone(), continued, &mut Vec::new())
            .await
            .unwrap();
        let sessions = kernel
            .list_sessions(SessionFilter {
                workspace: options().workspace,
                limit: 10,
            })
            .await
            .unwrap();
        assert!(sessions.is_empty(), "{prompt}: {sessions:?}");
    }
}

/// `-p` 的 `/rename`（非交互的 `rename`，titles.md §8.6）：不带名字时模型起名，带名字时直接改名；
/// 输出回执的文本，不开回合。
#[tokio::test]
async fn print_mode_renames_the_session() {
    // 新会话没有对话：给出说明，不发请求（脚本模型是空的）。
    let mut fresh = options();
    fresh.prompt = "/rename".to_owned();
    let mut output = Vec::new();
    run(kernel(ScriptedModel::new(Vec::new())), fresh, &mut output)
        .await
        .unwrap();
    assert_eq!(
        String::from_utf8(output).unwrap(),
        "Could not generate a name: no conversation context yet. Usage: /rename <name>\n"
    );
    let kernel = kernel(ScriptedModel::new(vec![
        text_response(&["first"]),
        text_response(&["{\"name\":\"first-session\"}"]),
    ]));
    run(kernel.clone(), options(), &mut Vec::new())
        .await
        .unwrap();
    let mut named = options();
    named.target = SessionTarget::Continue;
    named.prompt = " /rename ".to_owned();
    let mut output = Vec::new();
    run(kernel.clone(), named, &mut output).await.unwrap();
    assert_eq!(
        String::from_utf8(output).unwrap(),
        "Session renamed to: first-session\n"
    );
    let mut explicit = options();
    explicit.target = SessionTarget::Continue;
    explicit.prompt = "/name  my session ".to_owned();
    let mut output = Vec::new();
    run(kernel.clone(), explicit, &mut output).await.unwrap();
    assert_eq!(
        String::from_utf8(output).unwrap(),
        "Session renamed to: my session\n"
    );
    let latest = kernel
        .list_sessions(SessionFilter {
            workspace: options().workspace,
            limit: 1,
        })
        .await
        .unwrap();
    // 标题的 owner 是会话 actor（内存存储的列表不重放标题）。
    let view = kernel
        .open_session(&latest[0].id)
        .await
        .unwrap()
        .view()
        .await
        .unwrap();
    assert_eq!(view.meta.title, "my session");
}

/// `-p "/context"`（context-usage.md §1）：写 markdown 表，不建会话、不发模型请求。
#[tokio::test]
async fn context_prints_the_table_without_creating_a_session() {
    let kernel = kernel(ScriptedModel::new(Vec::new()));
    let mut output = Vec::new();
    let options = PrintOptions {
        session_id: None,
        prompt: "/context".to_owned(),
        ..options()
    };
    run(kernel.clone(), options.clone(), &mut output)
        .await
        .unwrap();
    let text = String::from_utf8(output).unwrap();
    assert!(
        text.starts_with("## Context Usage\n\n**Model:** scripted  \n"),
        "{text}"
    );
    assert!(text.contains("| System prompt | "), "{text}");
    assert!(
        text.ends_with("\n\n\n") || text.ends_with("|\n\n"),
        "{text:?}"
    );
    let sessions = kernel
        .list_sessions(SessionFilter {
            workspace: options.workspace,
            limit: 10,
        })
        .await
        .unwrap();
    assert!(sessions.is_empty());
}

/// `/agents` 已按用户决定删除（tui-display.md §3.2，L33）：`-p "/agents"` 不再是本地命令，照常建会话、发给
/// 模型（未知命令的提醒由 ext 的命令展开给出，ext.md §2.7）。
#[tokio::test]
async fn agents_is_no_longer_a_local_command() {
    let kernel = kernel(ScriptedModel::new(vec![text_response(&["ok"])]));
    let mut output = Vec::new();
    let options = PrintOptions {
        session_id: None,
        prompt: "/agents".to_owned(),
        ..options()
    };
    run(kernel.clone(), options.clone(), &mut output)
        .await
        .unwrap();
    assert_eq!(String::from_utf8(output).unwrap(), "ok\n");
    let sessions = kernel
        .list_sessions(SessionFilter {
            workspace: options.workspace,
            limit: 10,
        })
        .await
        .unwrap();
    assert_eq!(sessions.len(), 1);
}

/// 一直不结束的响应：开头的文字之后不再有事件，直到被取消；被请求时通知测试。
struct Stalled(Arc<tokio::sync::Notify>);

impl rust_cli_kernel_api::model::ModelPort for Stalled {
    fn stream(
        &self,
        _request: rust_cli_kernel_api::model::ModelRequest,
        cancel: CancellationToken,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<rust_cli_kernel_api::model::ModelEvent, ModelError>,
    > {
        use rust_cli_kernel_api::model::{BlockDelta, ModelEvent};
        self.0.notify_one();
        let head = vec![
            Ok(ModelEvent::MessageStart {
                id: "msg_api".to_owned(),
                model: "scripted".to_owned(),
                usage: Default::default(),
            }),
            Ok(ModelEvent::BlockStart {
                index: 0,
                block: ContentBlock::Text {
                    text: String::new(),
                },
            }),
            Ok(ModelEvent::BlockDelta {
                index: 0,
                delta: BlockDelta::Text("working".to_owned()),
            }),
        ];
        futures_util::stream::iter(head)
            .chain(futures_util::stream::pending())
            .take_until(cancel.cancelled_owned())
            .boxed()
    }
}

/// 退出信号（R5-2，session-lifecycle.md §3.5）：中止在跑的回合、等它结束后返回，stdout 不写回复。修复线 F38
/// 区分两种：SIGINT（`cancel` 先于 `stop` 取消，以 user-cancel 中止）留下流出的半句（query.md §1.4）与中断
/// 标记；SIGTERM / SIGHUP（只有 `stop`，不中止 query）以 `Shutdown` 中止，半句与标记都不写。F37 时两种都写标记。
#[tokio::test]
async fn a_stop_signal_interrupts_the_turn_and_prints_nothing() {
    for user_cancel in [true, false] {
        let requested = Arc::new(tokio::sync::Notify::new());
        let store = Arc::new(MemoryStore::default());
        let kernel = Arc::new(LocalKernel::new(
            Ports {
                model: Arc::new(Stalled(requested.clone())),
                environments: environments(Arc::new(UnusedEnvironment)),
                store: store.clone(),
                artifacts: store.clone(),
                clock: Arc::new(FixedClock(0)),
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
        ));
        let (stop, cancel) = (CancellationToken::new(), CancellationToken::new());
        let mut options = options();
        options.stop = stop.clone();
        options.cancel = cancel.clone();
        let mut output = Vec::new();
        let signal = async {
            requested.notified().await;
            if user_cancel {
                cancel.cancel();
            }
            stop.cancel();
        };
        let (result, ()) = tokio::join!(run(kernel.clone(), options, &mut output), signal);
        result.unwrap();
        assert!(output.is_empty());
        let session = kernel
            .list_sessions(SessionFilter {
                workspace: options_workspace(),
                limit: 1,
            })
            .await
            .unwrap()
            .remove(0)
            .id;
        let facts = format!("{:?}", store.facts(&session));
        assert_eq!(
            facts.contains("[Request interrupted by user]"),
            user_cancel,
            "{facts}"
        );
        assert_eq!(facts.contains("working"), user_cancel, "{facts}");
    }
}

fn options_workspace() -> WorkspaceRef {
    options().workspace
}
