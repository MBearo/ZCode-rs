//! Bash 工具经执行环境端口的流程：超时上限、cwd 范围、后台立即返回、取消是一条消息。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};
use rust_cli_kernel_api::env::{
    CommandEvent, EnvError, EnvFacts, FileContent, OperationContext, Origin, OutputStream,
    ReadFile, WorkspaceRef, WriteFile, WriteReceipt,
};
use rust_cli_kernel_api::env_shell::ShellState;
use rust_cli_kernel_api::ids::{
    AgentId, EnvironmentId, Generation, OperationId, OwnerEpoch, RunId, SessionId, ToolUseId,
};
use rust_cli_kernel_api::local_services::{Fetch, Git};
use rust_cli_kernel_api::machine::{
    Background, Checkpoints, Commands, EnvInfo, Files, Pdf, Search, Shell,
};
use rust_cli_kernel_api::message::ToolResultContent;
use rust_cli_kernel_api::orchestration::{
    InteractionAnswer, InteractionRequest, Interactions, OrchestrationError,
};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::{MemoryStore, local_services, machine};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use super::*;

/// 记下请求、按剧本回事件的执行环境。`hold` 为 true 时终态要等取消。
#[derive(Default)]
struct Scripted {
    started: Mutex<Vec<StartCommand>>,
    cancelled: Mutex<Vec<OperationId>>,
    output: Vec<u8>,
    exit_code: i32,
    hold: bool,
    /// 有值时终态是“输出超过上限被结束”（`Cancelled`、137 与这段说明）。
    output_limit: Option<String>,
    released: Arc<Notify>,
    /// 为 true 时输出之后是 `Backgrounded`（到了超时、转到后台），之后没有终态。
    backgrounded: bool,
}

#[async_trait]
impl EnvInfo for Scripted {
    async fn facts(&self, _workspace: &WorkspaceRef) -> Result<EnvFacts, EnvError> {
        Err(EnvError::Unsupported {
            what: "facts".to_owned(),
        })
    }
}

#[async_trait]
impl Files for Scripted {
    async fn read_file(&self, _w: &WorkspaceRef, _r: ReadFile) -> Result<FileContent, EnvError> {
        Err(EnvError::Unsupported {
            what: "read".to_owned(),
        })
    }

    async fn write_file(
        &self,
        _w: &WorkspaceRef,
        _c: OperationContext,
        _r: WriteFile,
    ) -> Result<WriteReceipt, EnvError> {
        Err(EnvError::Unsupported {
            what: "write".to_owned(),
        })
    }
}

#[async_trait]
impl Commands for Scripted {
    async fn start_command(
        &self,
        _w: &WorkspaceRef,
        _c: OperationContext,
        request: StartCommand,
    ) -> Result<(), EnvError> {
        self.started.lock().unwrap().push(request);
        Ok(())
    }

    fn watch_command(
        &self,
        _operation: OperationId,
        _cursor: u64,
    ) -> BoxStream<'static, Result<CommandEvent, EnvError>> {
        let first = [
            Ok(CommandEvent::Started {
                output: EnvPath::new("/t/b.output"),
            }),
            Ok(CommandEvent::Output {
                stream: OutputStream::Stdout,
                data: self.output.clone(),
                cursor: self.output.len() as u64,
            }),
        ];
        let finished = CommandEvent::Finished {
            outcome: if self.hold || self.output_limit.is_some() {
                Outcome::Cancelled
            } else {
                Outcome::Completed
            },
            exit_code: Some(self.exit_code),
            timed_out: false,
            cwd_reset: None,
            output_bytes: self.output.len() as u64,
            output_limit: self.output_limit.clone(),
        };
        if self.backgrounded {
            let moved = stream::iter(first).chain(stream::iter([Ok(CommandEvent::Backgrounded)]));
            return moved.chain(stream::pending()).boxed();
        }
        let released = self.released.clone();
        let hold = self.hold;
        let last = stream::once(async move {
            if hold {
                released.notified().await;
            }
            Ok(finished)
        });
        stream::iter(first).chain(last).boxed()
    }

    async fn cancel(&self, operation: OperationId) -> Result<(), EnvError> {
        self.cancelled.lock().unwrap().push(operation);
        self.released.notify_one();
        Ok(())
    }

    async fn operation_status(&self, _operation: OperationId) -> Result<Outcome, EnvError> {
        Ok(Outcome::Unknown)
    }

    async fn close_session(&self, _session: &SessionId, _forget: bool) -> Result<(), EnvError> {
        Ok(())
    }
}

#[async_trait]
impl Shell for Scripted {
    async fn shell_state(&self, _w: &WorkspaceRef, _s: &SessionId) -> Result<ShellState, EnvError> {
        Ok(ShellState {
            cwd: EnvPath::new("/w/sub"),
            outputs: None,
            background: Vec::new(),
        })
    }
}

impl Search for Scripted {}
impl Background for Scripted {}
impl Checkpoints for Scripted {}
impl Pdf for Scripted {}
impl Git for Scripted {}
impl Fetch for Scripted {}

struct Unused;

#[async_trait]
impl Interactions for Unused {
    async fn ask(
        &self,
        _s: &SessionId,
        _r: InteractionRequest,
    ) -> Result<InteractionAnswer, OrchestrationError> {
        Err(OrchestrationError::Unsupported {
            what: "ask".to_owned(),
        })
    }
}

fn context(env: Arc<Scripted>, agent: Option<AgentId>) -> ToolUseContext {
    ToolUseContext {
        session: SessionId::new("s1"),
        run: RunId::new("r1"),
        tool_use: ToolUseId::new("toolu_1"),
        agent,
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/w"),
        },
        mode: PermissionMode::Default,
        cancel: CancellationToken::new(),
        machine: machine(env.clone()),
        local: local_services(env),
        artifacts: Arc::new(MemoryStore::default()),
        interactions: Arc::new(Unused),
        operation: OperationContext {
            environment: EnvironmentId::new("local"),
            generation: Generation::new("g"),
            owner_epoch: OwnerEpoch::LOCAL,
            operation: OperationId::new("op_1"),
            origin: Origin::Agent {
                session: SessionId::new("s1"),
                run: RunId::new("r1"),
            },
        },
    }
}

async fn call(
    env: Arc<Scripted>,
    input: Value,
    agent: Option<AgentId>,
) -> Result<ToolOutput, ToolError> {
    let (progress, _updates) = mpsc::channel(8);
    BashTool::new()
        .call(input, context(env, agent), progress)
        .await
}

fn text_of(output: &ToolOutput) -> &str {
    match output.content.as_slice() {
        [ToolResultContent::Text { text }] => text,
        _ => panic!("not text"),
    }
}

#[tokio::test]
async fn foreground_commands_use_clamped_timeouts_and_the_session_cwd() {
    let env = Arc::new(Scripted {
        output: b"hi\n".to_vec(),
        ..Scripted::default()
    });
    let output = call(
        env.clone(),
        json!({"command": "echo hi", "timeout": 9_000_000}),
        None,
    )
    .await
    .unwrap();
    assert_eq!(text_of(&output), "hi");
    let start = env.started.lock().unwrap()[0].clone();
    assert_eq!(start.timeout_ms, Some(600_000));
    assert_eq!(
        start.cwd_scope,
        CwdScope::Session {
            roots: vec![EnvPath::new("/w")]
        }
    );
    assert!(start.background.is_none() && start.sandbox.is_none());
    assert_eq!(start.task, output::task_id(&OperationId::new("op_1")));
    call(env.clone(), json!({"command": "ls"}), None)
        .await
        .unwrap();
    assert_eq!(env.started.lock().unwrap()[1].timeout_ms, Some(120_000));
}

#[tokio::test]
async fn subagents_run_in_the_current_cwd_without_moving_it() {
    let env = Arc::new(Scripted::default());
    call(
        env.clone(),
        json!({"command": "pwd"}),
        Some(AgentId::new("agent_1")),
    )
    .await
    .unwrap();
    let start = env.started.lock().unwrap()[0].clone();
    assert_eq!(
        (start.cwd.as_str(), start.cwd_scope),
        ("/w/sub", CwdScope::Pinned)
    );
}

#[tokio::test]
async fn background_commands_return_the_output_file_at_once() {
    let env = Arc::new(Scripted {
        hold: true,
        ..Scripted::default()
    });
    let input = json!({"command": "sleep 9", "description": "Wait", "run_in_background": true});
    let output = call(env.clone(), input, None).await.unwrap();
    let task = output::task_id(&OperationId::new("op_1"));
    assert_eq!(text_of(&output), output::backgrounded(&task, "/t/b.output"));
    let start = env.started.lock().unwrap()[0].clone();
    assert_eq!(start.timeout_ms, Some(600_000));
    assert_eq!(start.background.unwrap().description, "Wait");
    assert!(env.cancelled.lock().unwrap().is_empty());
}

/// 后台命令不改会话的当前目录：在当前目录启动、结束时不写回；命令里有 cd 时结果说明这一点。
#[tokio::test]
async fn background_commands_start_in_the_current_cwd_without_moving_it() {
    let hint = "Session cwd remains /w/sub; directory changes made by the backgrounded command do not apply to subsequent commands.";
    for (command, hinted) in [
        ("cd .. && make", true),
        ("make; pushd /tmp", true),
        ("nohup popd", true),
        ("echo cd", false),
        ("sleep 9", false),
    ] {
        let env = Arc::new(Scripted {
            hold: true,
            ..Scripted::default()
        });
        let input = json!({"command": command, "description": "Run", "run_in_background": true});
        let output = call(env.clone(), input, None).await.unwrap();
        let start = env.started.lock().unwrap()[0].clone();
        assert_eq!(
            (start.cwd.as_str(), start.cwd_scope),
            ("/w/sub", CwdScope::Pinned),
            "{command}"
        );
        let task = output::task_id(&OperationId::new("op_1"));
        let started = output::backgrounded(&task, "/t/b.output");
        let expected = if hinted {
            format!("{started}\n{hint}")
        } else {
            started
        };
        assert_eq!(text_of(&output), expected, "{command}");
    }
}

#[tokio::test]
async fn nonzero_exits_are_errors_unless_the_command_says_otherwise() {
    let env = Arc::new(Scripted {
        output: b"boom\n".to_vec(),
        exit_code: 2,
        ..Scripted::default()
    });
    let failed = call(env, json!({"command": "make"}), None).await;
    assert_eq!(
        failed,
        Err(ToolError::Failed {
            message: "Exit code 2\nboom\n".to_owned()
        })
    );
    let env = Arc::new(Scripted {
        exit_code: 1,
        ..Scripted::default()
    });
    let ok = call(env, json!({"command": "grep x f"}), None)
        .await
        .unwrap();
    assert_eq!(text_of(&ok), "");
}

#[tokio::test]
async fn cancelling_sends_a_cancel_and_waits_for_the_terminal_state() {
    let env = Arc::new(Scripted {
        hold: true,
        ..Scripted::default()
    });
    let context = context(env.clone(), None);
    let cancel = context.cancel.clone();
    let (progress, _updates) = mpsc::channel(8);
    let tool = BashTool::new();
    let running = tool.call(json!({"command": "sleep 9"}), context, progress);
    cancel.cancel();
    assert_eq!(running.await, Err(ToolError::Cancelled));
    assert_eq!(
        env.cancelled.lock().unwrap().as_slice(),
        [OperationId::new("op_1")]
    );
}

#[test]
fn definition_and_flags_come_from_the_prompts_and_the_read_only_check() {
    let tool = BashTool::new();
    assert!(
        tool.description(&PromptContext {
            tool_names: Vec::new(),
            mode: PermissionMode::Default
        })
        .starts_with("Executes a bash command")
    );
    assert_eq!(tool.input_schema()["required"][0], "command");
    assert!(tool.is_concurrency_safe(&json!({"command": "git status"})));
    assert!(!tool.is_read_only(&json!({"command": "rm -rf x"})));
    assert_eq!(
        tool.interrupt_behavior(),
        rust_cli_kernel_api::tool::InterruptBehavior::Block
    );
}

/// 启动参数点名了 Glob / Grep 时它们可见，Bash 提示点名 `find`、`grep`；
/// 不可见时（缺省）不点名。
#[test]
fn the_description_names_find_and_grep_only_when_the_search_tools_are_visible() {
    let tool = BashTool::new();
    let describe = |names: &[&str]| {
        tool.description(&PromptContext {
            tool_names: names.iter().map(|name| (*name).to_owned()).collect(),
            mode: PermissionMode::Default,
        })
    };
    let without = describe(&["Bash", "Read"]);
    let with = describe(&["Bash", "Glob", "Grep", "Read"]);
    let lean =
        "Avoid using this tool to run `cat`, `head`, `tail`, `sed`, `awk`, or `echo` commands";
    let full = "Avoid using this tool to run `find`, `grep`, `cat`, `head`, `tail`, `sed`, `awk`, or `echo` commands";
    assert!(without.contains(lean) && !without.contains(full));
    assert!(with.contains(full));
    assert_eq!(with.replacen(full, lean, 1), without);
    assert_eq!(describe(&["Grep"]), with);
}

/// 修复线 F43（shell-permissions.md §1.1、§4.9）：进程启动时是 bypass 类别时，描述去掉 `- IMPORTANT: Avoid using this
/// tool to run …` 一行。两种点名写法都去掉，只差这一行；
/// 描述不随每一步的模式变（在会话里算一次）。
#[test]
fn a_bypass_launch_trims_the_avoid_cat_line() {
    let trimmed = BashTool::new().with_bash_first_trimmed(true);
    let plain = BashTool::new().with_bash_first_trimmed(false);
    let describe = |tool: &BashTool, names: &[&str], mode: PermissionMode| {
        tool.description(&PromptContext {
            tool_names: names.iter().map(|name| (*name).to_owned()).collect(),
            mode,
        })
    };
    let line = "\n- IMPORTANT: Avoid using this tool to run ";
    for names in [&["Bash", "Read"][..], &["Bash", "Glob", "Grep", "Read"][..]] {
        for mode in [PermissionMode::Default, PermissionMode::BypassPermissions] {
            let full = describe(&plain, names, mode);
            assert_eq!(full, describe(&BashTool::new(), names, mode));
            let start = full.find(line).unwrap();
            let end = start + 1 + full[start + 1..].find('\n').unwrap();
            let expected = format!("{}{}", &full[..start], &full[end..]);
            assert_eq!(describe(&trimmed, names, mode), expected);
            assert!(!expected.contains("Avoid using this tool"));
            assert!(expected.contains(
                "the shell is initialized from the user's profile.\n- Command output is displayed to you, not reliably to the user.\n"
            ));
        }
    }
    assert_eq!(
        describe(&plain, &["Bash"], PermissionMode::Default),
        describe(&plain, &["Bash"], PermissionMode::BypassPermissions)
    );
}

/// 修 bug（修复线 F9）：交互会话没有后台期限（只在 `-p` 时有，shell-permissions.md
/// §1.1）：描述与 schema 不写期限，后台命令不设超时；前台命令照旧。
#[tokio::test]
async fn interactive_sessions_have_no_background_deadline() {
    let tool = BashTool::new().with_background_deadline(false);
    let context = PromptContext {
        tool_names: vec!["Bash".to_owned()],
        mode: PermissionMode::Default,
    };
    let description = tool.description(&context);
    assert!(description.contains(
        "- `run_in_background` runs the command detached: it keeps running across turns and re-invokes you when it exits. No `&` needed.\n"
    ));
    assert!(!description.contains("With it, `timeout`"));
    assert_eq!(
        tool.input_schema()["properties"]["run_in_background"]["description"],
        "Set to true to run this command in the background."
    );
    // `-p`（缺省）仍是缺省的写法。
    let print = BashTool::new();
    assert!(
        print
            .description(&context)
            .contains("With it, `timeout` is how long")
    );
    assert_eq!(
        print.input_schema()["properties"]["run_in_background"]["description"],
        "Set to true to run this command in the background. With it, `timeout` limits how long the command may run in the background before it is stopped (default 600000 ms, max 7200000 ms)."
    );

    let env = Arc::new(Scripted {
        hold: true,
        ..Scripted::default()
    });
    let input = json!({"command": "sleep 9", "description": "Wait", "run_in_background": true, "timeout": 5000});
    let (progress, _updates) = mpsc::channel(8);
    tool.call(input, context_for(env.clone()), progress)
        .await
        .unwrap();
    let start = env.started.lock().unwrap()[0].clone();
    assert_eq!(start.timeout_ms, None);
    let env = Arc::new(Scripted::default());
    let (progress, _updates) = mpsc::channel(8);
    tool.call(
        json!({"command": "ls", "timeout": 5000}),
        context_for(env.clone()),
        progress,
    )
    .await
    .unwrap();
    assert_eq!(env.started.lock().unwrap()[0].timeout_ms, Some(5000));
}

fn context_for(env: Arc<Scripted>) -> ToolUseContext {
    context(env, None)
}

/// bash 模式（tui-input.md §2.2）：不套 sandbox；成功的输出进 stdout（转义），失败的退出码把合流的输出放进
/// stderr（不带 `Exit code N`），取消是 Interrupted。
#[tokio::test]
async fn bash_mode_splits_stdout_and_stderr() {
    let (progress, _updates) = mpsc::channel(8);
    let env = Arc::new(Scripted {
        output: b"<a> & b\n".to_vec(),
        ..Scripted::default()
    });
    let bash = BashTool::new().with_sandbox(SandboxPolicy {
        filesystem: rust_cli_kernel_api::env_shell::FilesystemPolicy::default(),
        network: rust_cli_kernel_api::env_shell::NetworkPolicy::default(),
        required: true,
    });
    let done = crate::bash_mode::run(
        &bash,
        "echo hi",
        context(env.clone(), None),
        progress.clone(),
    )
    .await;
    assert_eq!(
        done,
        rust_cli_kernel_api::user_input::BashRun::Completed {
            stdout: "&lt;a&gt; &amp; b".to_owned(),
            stderr: String::new(),
        }
    );
    assert!(env.started.lock().unwrap()[0].sandbox.is_none());
    let failing = Arc::new(Scripted {
        output: b"ls: nope: No such file\n".to_vec(),
        exit_code: 1,
        ..Scripted::default()
    });
    let failed =
        crate::bash_mode::run(&bash, "ls nope", context(failing, None), progress.clone()).await;
    assert_eq!(
        failed,
        rust_cli_kernel_api::user_input::BashRun::Completed {
            stdout: String::new(),
            stderr: "ls: nope: No such file\n".to_owned(),
        }
    );
    let held = Arc::new(Scripted {
        hold: true,
        ..Scripted::default()
    });
    let ctx = context(held, None);
    ctx.cancel.cancel();
    let cancelled = crate::bash_mode::run(&bash, "sleep 9", ctx, progress).await;
    assert_eq!(
        cancelled,
        rust_cli_kernel_api::user_input::BashRun::Interrupted
    );
}

#[tokio::test]
async fn an_output_file_past_the_limit_reads_as_a_killed_command() {
    // F11（R2-18）：执行环境因输出文件超过上限结束命令时，说明放在输出前、退出码 137 按错误报告，
    // 没有中断提示（`interrupted` 为假）；旧代码把这种 `Cancelled` 当成用户取消。
    let env = Arc::new(Scripted {
        output: b"y\ny\n".to_vec(),
        exit_code: 137,
        output_limit: Some("Command killed: output file exceeded 5GB".to_owned()),
        ..Scripted::default()
    });
    let error = call(env.clone(), json!({"command": "yes"}), None)
        .await
        .unwrap_err();
    assert_eq!(
        error,
        ToolError::Failed {
            message: "Exit code 137\nCommand killed: output file exceeded 5GB\ny\ny\n".to_owned()
        }
    );
    assert!(env.cancelled.lock().unwrap().is_empty());
}

/// `BASH_*_TIMEOUT_MS`（cli-args.md §9.3，修复线 F42）：描述与 schema 里的数字跟着换，执行时用新的缺省与上限；
/// 与交互形态的去掉期限可以任意先后；缺省值时文字不变。
#[tokio::test]
async fn the_timeouts_change_the_text_and_the_execution() {
    let context = PromptContext {
        tool_names: vec!["Bash".to_owned()],
        mode: PermissionMode::Default,
    };
    let baseline = BashTool::new();
    let same = BashTool::new().with_timeouts(BashTimeouts::default());
    assert_eq!(same.description(&context), baseline.description(&context));
    assert_eq!(same.input_schema(), baseline.input_schema());

    let timeouts = BashTimeouts::new(Some(900_000), Some(8_000_000));
    let tool = BashTool::new().with_timeouts(timeouts);
    let description = tool.description(&context);
    assert!(description.contains(
        "- `timeout` is in milliseconds: default 900000, max 8000000 for a foreground command."
    ));
    assert!(description.contains("(default 900000, max 8000000); at that limit"));
    assert!(!description.contains("120000"));
    let schema = tool.input_schema();
    assert_eq!(
        schema["properties"]["timeout"]["description"],
        "Optional timeout in milliseconds (max 8000000 for a foreground command)"
    );
    assert!(
        schema["properties"]["run_in_background"]["description"]
            .as_str()
            .unwrap()
            .ends_with("(default 900000 ms, max 8000000 ms).")
    );
    // 交互形态：先后两种顺序结果相同，期限的说明都去掉。
    let first = BashTool::new()
        .with_timeouts(timeouts)
        .with_background_deadline(false);
    let second = BashTool::new()
        .with_background_deadline(false)
        .with_timeouts(timeouts);
    assert_eq!(first.description(&context), second.description(&context));
    assert_eq!(first.input_schema(), second.input_schema());
    assert!(!first.description(&context).contains("With it, `timeout`"));
    assert!(
        first
            .description(&context)
            .contains("default 900000, max 8000000 for a foreground")
    );

    let env = Arc::new(Scripted::default());
    let (progress, _updates) = mpsc::channel(8);
    tool.call(json!({"command": "ls"}), context_for(env.clone()), progress)
        .await
        .unwrap();
    let (progress, _updates) = mpsc::channel(8);
    tool.call(
        json!({"command": "ls", "timeout": 99_000_000}),
        context_for(env.clone()),
        progress,
    )
    .await
    .unwrap();
    let started = env.started.lock().unwrap().clone();
    assert_eq!(started[0].timeout_ms, Some(900_000));
    assert_eq!(started[1].timeout_ms, Some(8_000_000));
}

/// stream-json 输入的 `-p`（headless.md §6.1，修复线 F39）：后台命令的缺省期限是
/// 1800000（单次 `-p` 是 600000）；描述与 schema 的数字随之变。
#[tokio::test]
async fn streaming_input_gives_background_commands_thirty_minutes() {
    let tool = BashTool::new().with_streaming_input();
    let context = PromptContext {
        tool_names: vec!["Bash".to_owned()],
        mode: PermissionMode::Default,
    };
    assert!(tool.description(&context).contains(
        "With it, `timeout` is how long the command may run in the background (default 1800000, max 7200000);"
    ));
    assert_eq!(
        tool.input_schema()["properties"]["run_in_background"]["description"],
        "Set to true to run this command in the background. With it, `timeout` limits how long the command may run in the background before it is stopped (default 1800000 ms, max 7200000 ms)."
    );
    let env = Arc::new(Scripted {
        hold: true,
        ..Scripted::default()
    });
    let input = json!({"command": "sleep 9", "description": "Wait", "run_in_background": true});
    let (progress, _updates) = mpsc::channel(8);
    tool.call(input, context_for(env.clone()), progress)
        .await
        .unwrap();
    let start = env.started.lock().unwrap()[0].clone();
    assert_eq!(start.timeout_ms, Some(1_800_000));
}

/// stream-json 输入与 `BASH_DEFAULT_TIMEOUT_MS` 同时设（F39 与 F42 合并）：后台缺省是 max(1800000, 前台缺省)。
#[test]
fn streaming_input_keeps_the_configured_timeouts() {
    let tool = BashTool::new()
        .with_timeouts(BashTimeouts::new(Some(2_000_000), Some(8_000_000)))
        .with_streaming_input();
    let context = PromptContext {
        tool_names: vec!["Bash".to_owned()],
        mode: PermissionMode::Default,
    };
    let description = tool.description(&context);
    assert!(description.contains("default 2000000, max 8000000 for a foreground command."));
    assert!(description.contains("(default 2000000, max 8000000); at that limit"));
}

#[path = "bash_timeout_tests.rs"]
mod timeout;

#[path = "bash_remote_tests.rs"]
mod remote;
