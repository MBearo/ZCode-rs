//! 会话的当前目录（F19，docs/specs/rust-cli/tools-files.md §3.3）：Bash `cd` 之后，文件工具的相对路径与权限判定
//! 以同一个目录为基准。真实 env-local（外面套一层序列化往返）、真实判决链
//! （`PolicyGate`）、真实的 Bash 与文件工具，经执行管线调用。
//!
//! 修前：权限判定按 shell 的当前目录解析（`<根>/sub/x.txt`，放行），工具按工作区根写（`<根>/x.txt`，正是 deny
//! 规则保护的文件）——判的是 A，写的是 B。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录、读回写出的文件"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_env_local::{LocalEnvironment, ShellConfig};
use rust_cli_kernel_api::env::{EnvPath, OperationContext, Origin, WorkspaceRef};
use rust_cli_kernel_api::ids::{
    EnvironmentId, Generation, InteractionId, OperationId, OwnerEpoch, RunId, SessionId, ToolUseId,
};
use rust_cli_kernel_api::message::ToolResultContent;
use rust_cli_kernel_api::orchestration::{
    InteractionAnswer, InteractionRequest, Interactions, OrchestrationError, PermissionGate,
    ToolExecutor, ToolRegistry,
};
use rust_cli_kernel_api::permission::{
    PermissionMode, PermissionRules, RuleBehavior, RuleSource, SourcedRule,
};
use rust_cli_kernel_api::testing::{MemoryStore, NoHooks, RoundTripEnv};
use rust_cli_kernel_api::tool::{Tool, ToolUseContext};
use rust_cli_policy::PolicyGate;
use rust_cli_policy::rules::parse_rule;
use rust_cli_tools::{BashTool, BuiltinTools, Pipeline};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// `-p` 没有交互：询问按拒绝处理，询问的文本成为 tool_result。
struct NoOneToAsk;

#[async_trait]
impl Interactions for NoOneToAsk {
    async fn ask(
        &self,
        _session: &SessionId,
        _request: InteractionRequest,
    ) -> Result<InteractionAnswer, OrchestrationError> {
        Ok(InteractionAnswer {
            interaction: InteractionId::new("i"),
            answer: json!({"decision": "unavailable"}),
        })
    }
}

/// 一个工作区、一套工具与一条判决链（readFileState 在工具里，同一个会话的各步共用）。
struct Bench {
    _dir: tempfile::TempDir,
    root: PathBuf,
    tools: BuiltinTools,
    bash: Arc<dyn Tool>,
    pipeline: Pipeline,
    env: Arc<RoundTripEnv>,
    mode: PermissionMode,
    step: usize,
}

impl Bench {
    /// `rules` 是命令行来源的规则（`--allowedTools` / `--disallowedTools`）。
    fn new(mode: PermissionMode, rules: &[(RuleBehavior, &str)]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let root = base.join("work");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(base.join("home")).unwrap();
        std::fs::write(root.join("a.txt"), "alpha\n").unwrap();
        std::fs::write(root.join("sub/b.txt"), "bravo\n").unwrap();
        let shell = ShellConfig {
            scratch: base.join("scratch"),
            ledger: base.join("ledger"),
            bash: Some(PathBuf::from("/bin/bash")),
            bwrap: None,
            embedded: None,
            env: vec![
                ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
                (
                    "HOME".to_owned(),
                    base.join("home").to_string_lossy().into_owned(),
                ),
            ]
            .into(),
        };
        let local = LocalEnvironment::new(Some(base.join("home"))).with_shell(shell);
        let gate: Arc<dyn PermissionGate> = Arc::new(PolicyGate::new(PermissionRules {
            version: 1,
            rules: rules
                .iter()
                .map(|(behavior, text)| SourcedRule {
                    source: RuleSource::CliArg,
                    behavior: *behavior,
                    rule: parse_rule(text),
                })
                .collect(),
            ..PermissionRules::default()
        }));
        Self {
            _dir: dir,
            root,
            tools: rust_cli_tools::builtin(gate.clone()).with_search_opt_in(true),
            bash: Arc::new(BashTool::new()),
            pipeline: Pipeline::new(gate, Arc::new(NoHooks)),
            env: Arc::new(RoundTripEnv(Arc::new(local))),
            mode,
            step: 0,
        }
    }

    fn path(&self, name: &str) -> String {
        format!("{}/{name}", self.root.display())
    }

    /// 跑一次工具，返回 tool_result 的文本与 is_error。
    async fn call(&mut self, tool: &str, input: Value) -> (String, bool) {
        self.step += 1;
        let session = SessionId::new("00000000-0000-4000-8000-000000000001");
        let context = ToolUseContext {
            session: session.clone(),
            run: RunId::new("run_1"),
            tool_use: ToolUseId::new(format!("toolu_{:03}", self.step)),
            agent: None,
            workspace: WorkspaceRef {
                environment: EnvironmentId::new("local"),
                root: EnvPath::new(self.root.to_string_lossy()),
            },
            mode: self.mode,
            cancel: CancellationToken::new(),
            env: self.env.clone(),
            artifacts: Arc::new(MemoryStore::default()),
            interactions: Arc::new(NoOneToAsk),
            operation: OperationContext {
                environment: EnvironmentId::new("local"),
                generation: Generation::new("g1"),
                owner_epoch: OwnerEpoch::LOCAL,
                operation: OperationId::new(format!("op_{}", self.step)),
                origin: Origin::Agent {
                    session,
                    run: RunId::new("run_1"),
                },
            },
        };
        let tool = if tool == "Bash" {
            self.bash.clone()
        } else {
            self.tools.find(tool).unwrap()
        };
        let (progress, _updates) = mpsc::channel(64);
        let output = self.pipeline.execute(tool, input, context, progress).await;
        let text = output
            .content
            .iter()
            .filter_map(|block| match block {
                ToolResultContent::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        (text, output.is_error)
    }
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

#[tokio::test]
async fn write_after_cd_lands_on_the_file_the_permission_check_judged() {
    // deny 规则保护工作区根下的 x.txt（`/x` 相对规则的根，即工作区根）；acceptEdits 下工作目录里的写入直接放行。
    let mut bench = Bench::new(
        PermissionMode::AcceptEdits,
        &[(RuleBehavior::Deny, "Edit(/x.txt)")],
    );
    let (text, error) = bench.call("Bash", json!({"command": "cd sub"})).await;
    assert!(!error, "{text}");

    let (text, error) = bench
        .call(
            "Write",
            json!({"file_path": "x.txt", "content": "in sub\n"}),
        )
        .await;
    assert!(!error, "{text}");
    // 结果里是输入的原样路径（`filePath` 是输入）；落在哪里看磁盘。
    assert!(
        text.starts_with("File created successfully at: x.txt"),
        "{text}"
    );
    assert_eq!(
        read(&bench.root.join("sub/x.txt")).as_deref(),
        Some("in sub\n")
    );
    // 修前：判决链看的是 sub/x.txt（放行），工具写的是被 deny 规则保护的根下 x.txt。
    assert_eq!(read(&bench.root.join("x.txt")), None);

    // 同一个文件写成 `../x.txt`：判决链与工具都落在根下的 x.txt，被拒。
    let (text, error) = bench
        .call(
            "Write",
            json!({"file_path": "../x.txt", "content": "denied\n"}),
        )
        .await;
    assert!(error);
    assert_eq!(
        text,
        format!(
            "Permission to edit {} has been denied.",
            bench.path("x.txt")
        )
    );
    assert_eq!(read(&bench.root.join("x.txt")), None);

    // Edit 与 Read 同样以 sub 为基准；Edit 认得出 Write 记下的 readFileState（同一个绝对路径）。
    let (text, error) = bench
        .call(
            "Edit",
            json!({"file_path": "x.txt", "old_string": "in sub", "new_string": "edited"}),
        )
        .await;
    assert!(!error, "{text}");
    assert!(
        text.starts_with("The file x.txt has been updated successfully."),
        "{text}"
    );
    assert_eq!(
        read(&bench.root.join("sub/x.txt")).as_deref(),
        Some("edited\n")
    );
    let (text, error) = bench.call("Read", json!({"file_path": "b.txt"})).await;
    assert!(!error && text.contains("1\tbravo"), "{text}");
}

#[tokio::test]
async fn search_tools_and_their_messages_use_the_shell_directory() {
    let mut bench = Bench::new(PermissionMode::DontAsk, &[]);
    bench.call("Bash", json!({"command": "cd sub"})).await;

    // Glob / Grep 不给 path 时在当前目录里找，结果相对当前目录显示。
    let (text, error) = bench.call("Glob", json!({"pattern": "*.txt"})).await;
    assert!(!error, "{text}");
    assert_eq!(text, "b.txt");
    let (text, error) = bench.call("Grep", json!({"pattern": "bravo"})).await;
    assert!(!error, "{text}");
    assert_eq!(text, "Found 1 file\nb.txt");

    // 报错里的工作目录是 shell 的当前目录。
    let (text, error) = bench
        .call("Read", json!({"file_path": "missing.txt"}))
        .await;
    assert!(error);
    assert!(
        text.contains(&format!(
            "Note: your current working directory is {}.",
            bench.path("sub")
        )),
        "{text}"
    );
}

#[tokio::test]
async fn cd_outside_the_workspace_resets_and_relative_paths_follow_the_reset() {
    // `cd /` 是工作区外的路径，要 Bash 的 allow 规则才不询问。
    let mut bench = Bench::new(
        PermissionMode::AcceptEdits,
        &[(RuleBehavior::Allow, "Bash")],
    );
    let (text, _) = bench.call("Bash", json!({"command": "cd sub"})).await;
    assert!(!text.contains("reset"), "{text}");
    // 工作区外：cwd 改回原来的目录（工作区根）并在结果里说明。
    let (text, error) = bench.call("Bash", json!({"command": "cd / && pwd"})).await;
    assert!(!error, "{text}");
    assert_eq!(
        text,
        format!("/\nShell cwd was reset to {}", bench.root.display())
    );

    let (text, error) = bench
        .call("Write", json!({"file_path": "y.txt", "content": "root\n"}))
        .await;
    assert!(!error, "{text}");
    assert_eq!(read(&bench.root.join("y.txt")).as_deref(), Some("root\n"));
    assert_eq!(read(&bench.root.join("sub/y.txt")), None);
    let (text, error) = bench.call("Read", json!({"file_path": "a.txt"})).await;
    assert!(!error && text.contains("1\talpha"), "{text}");
}

/// 后台命令里的 `cd` 不改会话的当前目录（shell-permissions.md §1.2）：后台命令什么时候结束不确定，以前结束时把
/// cwd 写回会话，之后任意两个工具调用之间会话的当前目录都可能跳走。现在结束之后 `pwd` 仍是原来的目录。
#[tokio::test]
async fn a_background_cd_does_not_move_the_session_directory() {
    let mut bench = Bench::new(
        PermissionMode::AcceptEdits,
        &[(RuleBehavior::Allow, "Bash")],
    );
    let (text, error) = bench
        .call(
            "Bash",
            json!({"command": "cd sub && pwd", "description": "Go", "run_in_background": true}),
        )
        .await;
    assert!(!error, "{text}");
    assert!(
        text.ends_with(&format!(
            "\nSession cwd remains {}; directory changes made by the backgrounded command do not apply to subsequent commands.",
            bench.root.display()
        )),
        "{text}"
    );
    // 等后台命令写完终态（输出文件末尾的 `[exited with code N]`）。
    let output = text
        .split("Output is being written to: ")
        .nth(1)
        .and_then(|rest| rest.split(". You will be notified").next())
        .unwrap()
        .to_owned();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if read(Path::new(&output)).is_some_and(|text| text.contains("[exited with code")) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the background command did not finish"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(
        read(Path::new(&output)).unwrap(),
        format!("{}\n\n[exited with code 0]\n", bench.path("sub"))
    );
    let (text, error) = bench.call("Bash", json!({"command": "pwd"})).await;
    assert!(!error, "{text}");
    assert_eq!(text, bench.root.display().to_string());
    // 后台命令从会话的当前目录启动。
    bench.call("Bash", json!({"command": "cd sub"})).await;
    let (text, _) = bench
        .call(
            "Bash",
            json!({"command": "pwd > where.txt", "description": "Where", "run_in_background": true}),
        )
        .await;
    assert!(!text.contains("Session cwd remains"), "{text}");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while read(&bench.root.join("sub/where.txt")).is_none_or(|text| text.is_empty()) {
        assert!(
            std::time::Instant::now() < deadline,
            "where.txt was not written"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(
        read(&bench.root.join("sub/where.txt")).unwrap(),
        format!("{}\n", bench.path("sub"))
    );
}
