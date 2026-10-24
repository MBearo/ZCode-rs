//! 工作目录的 owner 是会话 actor（docs/specs/rust-cli/tools-more.md §11.4）：工具改了目录后，同一回合里
//! 后续的工具调用立即在新目录，执行环境的 shell cwd 一并设好；执行环境拒绝时目录不变。

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use futures_util::stream::BoxStream;
use rust_cli_kernel_api::env::{
    CommandEvent, EnvError, EnvFacts, EnvPath, FileContent, OperationContext, Outcome, ReadFile,
    StartCommand, WorkspaceRef, WriteFile, WriteReceipt,
};
use rust_cli_kernel_api::ids::{OperationId, SessionId};
use rust_cli_kernel_api::local_services::{Fetch, Git};
use rust_cli_kernel_api::machine::{
    Background, Checkpoints, Commands, EnvInfo, Files, Pdf, Search, Shell,
};
use rust_cli_kernel_api::message::{ContentBlock, Message, ToolResultContent};
use rust_cli_kernel_api::session_api::SessionHandle;
use rust_cli_kernel_api::testing::{
    MemoryEnvironment, ScriptedModel, text_response, tool_use_response,
};
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::test_support::{Setup, env, finished, submit, until};

/// 内存执行环境，另外记下 shell cwd；`refuse` 时拒绝设置。
struct CwdEnv {
    inner: Arc<MemoryEnvironment>,
    cwd: Mutex<Vec<String>>,
    refuse: bool,
}

#[async_trait]
impl EnvInfo for CwdEnv {
    async fn facts(&self, workspace: &WorkspaceRef) -> Result<EnvFacts, EnvError> {
        self.inner.facts(workspace).await
    }
}

#[async_trait]
impl Files for CwdEnv {
    async fn read_file(
        &self,
        workspace: &WorkspaceRef,
        request: ReadFile,
    ) -> Result<FileContent, EnvError> {
        self.inner.read_file(workspace, request).await
    }

    async fn write_file(
        &self,
        workspace: &WorkspaceRef,
        context: OperationContext,
        request: WriteFile,
    ) -> Result<WriteReceipt, EnvError> {
        self.inner.write_file(workspace, context, request).await
    }
}

#[async_trait]
impl Commands for CwdEnv {
    async fn start_command(
        &self,
        workspace: &WorkspaceRef,
        context: OperationContext,
        request: StartCommand,
    ) -> Result<(), EnvError> {
        self.inner.start_command(workspace, context, request).await
    }

    fn watch_command(
        &self,
        operation: OperationId,
        cursor: u64,
    ) -> BoxStream<'static, Result<CommandEvent, EnvError>> {
        self.inner.watch_command(operation, cursor)
    }

    async fn cancel(&self, operation: OperationId) -> Result<(), EnvError> {
        self.inner.cancel(operation).await
    }

    async fn operation_status(&self, operation: OperationId) -> Result<Outcome, EnvError> {
        self.inner.operation_status(operation).await
    }

    async fn close_session(&self, session: &SessionId, forget: bool) -> Result<(), EnvError> {
        self.inner.close_session(session, forget).await
    }
}

#[async_trait]
impl Shell for CwdEnv {
    async fn set_shell_cwd(&self, _session: &SessionId, cwd: EnvPath) -> Result<(), EnvError> {
        if self.refuse {
            return Err(EnvError::Unsupported {
                what: "set_shell_cwd".to_owned(),
            });
        }
        self.cwd
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(cwd.as_str().to_owned());
        Ok(())
    }
}

impl Search for CwdEnv {}
impl Background for CwdEnv {}
impl Checkpoints for CwdEnv {}
impl Pdf for CwdEnv {}
impl Git for CwdEnv {}
impl Fetch for CwdEnv {}

/// `Switch` 请求改目录；`Where` 报告自己的工具上下文里的目录。
struct DirTool(&'static str);

#[async_trait]
impl Tool for DirTool {
    fn name(&self) -> &str {
        self.0
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }

    fn description(&self, _context: &PromptContext) -> String {
        String::new()
    }

    async fn call(
        &self,
        _input: Value,
        context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let text = if self.0 == "Switch" {
            match context
                .interactions
                .set_workspace(&context.session, EnvPath::new("/work/.zcode/worktrees/a"))
                .await
            {
                Ok(()) => "switched".to_owned(),
                Err(error) => error.to_string(),
            }
        } else {
            context.workspace.root.as_str().to_owned()
        };
        Ok(ToolOutput {
            content: vec![ToolResultContent::Text { text }],
            is_error: false,
            display: Value::Null,
        })
    }
}

async fn run(refuse: bool) -> (Vec<String>, Vec<String>, String) {
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "Switch", &json!({})),
        tool_use_response("toolu_2", "Where", &json!({})),
        text_response(&["done"]),
    ]));
    let cwd_env = Arc::new(CwdEnv {
        inner: env(),
        cwd: Mutex::default(),
        refuse,
    });
    let mut setup = Setup::new(model);
    setup.set_env(cwd_env.clone());
    setup.tools = vec![Arc::new(DirTool("Switch")), Arc::new(DirTool("Where"))];
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "go")).await;
    until(&mut events, finished).await;
    let view = built.session.view().await.unwrap();
    let results = view
        .messages
        .iter()
        .flat_map(|message| match message {
            Message::User(user) => user.content.clone(),
            _ => Vec::new(),
        })
        .filter_map(|block| match block {
            ContentBlock::ToolResult { content, .. } => match content.as_slice() {
                [ToolResultContent::Text { text }] => Some(text.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    let cwd = cwd_env
        .cwd
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    (results, cwd, view.meta.workspace.root.as_str().to_owned())
}

#[tokio::test]
async fn a_workspace_change_applies_to_the_next_tool_call_in_the_same_turn() {
    let (results, cwd, root) = run(false).await;
    assert_eq!(results, ["switched", "/work/.zcode/worktrees/a"]);
    assert_eq!(cwd, ["/work/.zcode/worktrees/a"]);
    assert_eq!(root, "/work/.zcode/worktrees/a");
}

#[tokio::test]
async fn a_refused_shell_cwd_keeps_the_old_workspace() {
    let (results, cwd, root) = run(true).await;
    assert_eq!(results.len(), 2);
    assert_eq!(results[1], "/work");
    assert!(cwd.is_empty());
    assert_eq!(root, "/work");
}

/// 放行之前一直不结束的模型（让回合停在进行中）。
struct Held(Arc<tokio::sync::Notify>);

impl rust_cli_kernel_api::model::ModelPort for Held {
    fn stream(
        &self,
        _request: rust_cli_kernel_api::model::ModelRequest,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> BoxStream<
        'static,
        Result<rust_cli_kernel_api::model::ModelEvent, rust_cli_kernel_api::model::ModelError>,
    > {
        use futures_util::StreamExt;
        let gate = self.0.clone();
        futures_util::stream::once(async move { gate.notified().await })
            .filter_map(|()| async { None })
            .chain(futures_util::stream::iter(text_response(&["ok"])))
            .boxed()
    }
}

/// `/cd`（tui-manage.md §7.1）：空闲时换目录并提交说明；进行中的回合里拒绝。
#[tokio::test]
async fn set_workspace_moves_an_idle_session_and_commits_the_note() {
    use rust_cli_kernel_api::message::UserOrigin;
    use rust_cli_kernel_api::session_api::{Ack, Command, KernelEvent};

    use crate::test_support::envelope;

    let gate = Arc::new(tokio::sync::Notify::new());
    let cwd_env = Arc::new(CwdEnv {
        inner: env(),
        cwd: Mutex::default(),
        refuse: false,
    });
    let mut setup = Setup::new(Arc::new(Held(gate.clone())));
    setup.set_env(cwd_env.clone());
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    let set = |id: &str| {
        envelope(
            id,
            Command::SetWorkspace {
                root: EnvPath::new("/work/other"),
                note: Some("<system-reminder>\nmoved\n</system-reminder>".to_owned()),
            },
        )
    };
    // 回合进行中：拒绝，目录不变。
    built.session.send(submit("c1", "go")).await;
    until(&mut events, |event| {
        matches!(event, KernelEvent::TurnStarted { .. })
    })
    .await;
    let Ack::Rejected { reason } = built.session.send(set("c2")).await else {
        panic!("a running turn must refuse the move");
    };
    assert!(reason.starts_with("A turn is in progress"), "{reason}");
    gate.notify_one();
    until(&mut events, finished).await;
    assert!(matches!(
        built.session.send(set("c3")).await,
        Ack::Accepted { .. }
    ));
    let view = built.session.view().await.unwrap();
    assert_eq!(view.meta.workspace.root.as_str(), "/work/other");
    let Some(Message::User(note)) = view.messages.last() else {
        panic!("no note");
    };
    assert_eq!(
        note.origin,
        UserOrigin::Meta {
            source: "rust-cli/cd".to_owned()
        }
    );
    assert_eq!(
        *cwd_env.cwd.lock().unwrap_or_else(PoisonError::into_inner),
        ["/work/other"]
    );
}

/// 复现（R1-3）：以前工具改了工作目录（EnterWorktree / ExitWorktree）不发事件，前端只能自存一份目录，只有 `/cd`
/// 才更新它——之后的 `/diff`、`@` 补全、`/export`、`/permissions` 用旧目录。现在 actor 改目录时发
/// `WorkspaceChanged`，前端只从事件与快照取。
#[tokio::test]
async fn a_workspace_change_is_announced_to_the_front_end() {
    use rust_cli_kernel_api::session_api::KernelEvent;

    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "Switch", &json!({})),
        text_response(&["done"]),
    ]));
    let cwd_env = Arc::new(CwdEnv {
        inner: env(),
        cwd: Mutex::default(),
        refuse: false,
    });
    let mut setup = Setup::new(model);
    setup.set_env(cwd_env);
    setup.tools = vec![Arc::new(DirTool("Switch"))];
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "go")).await;
    let seen = until(&mut events, finished).await;
    let changed: Vec<&str> = seen
        .iter()
        .filter_map(|event| match event {
            KernelEvent::WorkspaceChanged { workspace } => Some(workspace.root.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(changed, ["/work/.zcode/worktrees/a"]);
}

/// 规则文件来源的替身：新目录有一条规则。
struct OneRule;

#[async_trait]
impl rust_cli_kernel_api::orchestration::DirectoryRules for OneRule {
    async fn directory_rules(
        &self,
        workspace: &WorkspaceRef,
        history: &[Message],
    ) -> Option<String> {
        Some(format!(
            "rules for {} after {} messages",
            workspace.root.as_str(),
            history.len()
        ))
    }
}

/// `/cd` 整个由 actor 判定（R1-3）：与自己的工作目录相同时 `Unchanged`；移动时设 shell cwd、发事件、提交
/// 说明加新目录的规则文件（用 actor 自己的历史，不复制给调用方）；运行中按固定的文字拒绝。
#[tokio::test]
async fn change_directory_is_judged_by_the_actor() {
    use rust_cli_kernel_api::manage::DirectoryOutcome;
    use rust_cli_kernel_api::session_api::{Ack, Command, KernelEvent};

    use crate::test_support::envelope;

    let gate = Arc::new(tokio::sync::Notify::new());
    let cwd_env = Arc::new(CwdEnv {
        inner: env(),
        cwd: Mutex::default(),
        refuse: false,
    });
    let mut setup = Setup::new(Arc::new(Held(gate.clone())));
    setup.set_env(cwd_env.clone());
    setup.directory_rules = Arc::new(OneRule);
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    let cd = |id: &str, root: &str| {
        envelope(
            id,
            Command::ChangeDirectory {
                root: EnvPath::new(root),
            },
        )
    };
    let outcome = |ack: Ack| match ack {
        Ack::Accepted { result } => serde_json::from_value::<DirectoryOutcome>(result).unwrap(),
        other => panic!("{other:?}"),
    };
    assert_eq!(
        outcome(built.session.send(cd("c1", "/work")).await),
        DirectoryOutcome::Unchanged
    );
    // 回合进行中：拒绝（固定的文字），目录不变。
    built.session.send(submit("c2", "go")).await;
    until(&mut events, |event| {
        matches!(event, KernelEvent::TurnStarted { .. })
    })
    .await;
    let Ack::Rejected { reason } = built.session.send(cd("c3", "/work/other")).await else {
        panic!("a running turn must refuse the move");
    };
    assert!(reason.starts_with("A turn is in progress"), "{reason}");
    gate.notify_one();
    until(&mut events, finished).await;
    assert_eq!(
        outcome(built.session.send(cd("c4", "/work/other")).await),
        DirectoryOutcome::Moved
    );
    let seen = until(&mut events, |event| {
        matches!(event, KernelEvent::MessageDone { .. })
    })
    .await;
    assert!(seen.iter().any(|event| matches!(
        event,
        KernelEvent::WorkspaceChanged { workspace } if workspace.root.as_str() == "/work/other"
    )));
    let view = built.session.view().await.unwrap();
    assert_eq!(view.meta.workspace.root.as_str(), "/work/other");
    let Some(Message::User(note)) = view.messages.last() else {
        panic!("no note");
    };
    let ContentBlock::Text { text } = &note.content[0] else {
        panic!("not text");
    };
    assert!(
        text.starts_with(
            "<system-reminder>\nThe session's working directory has changed to /work/other (via /cd)."
        ),
        "{text}"
    );
    assert!(
        text.ends_with("\n\nrules for /work/other after 2 messages"),
        "{text}"
    );
    assert_eq!(
        *cwd_env.cwd.lock().unwrap_or_else(PoisonError::into_inner),
        ["/work/other"]
    );
}

/// 工作线 F57：`/cd` 的“已经在这里”用 kernel-api `env_path` 的键判断（规范化；Windows 上不分大小写），与没有会话时
/// runtime 的判断相同。以前按字符串 `==`：Windows 上 `/cd c:\work` 回到 `C:\Work` 被当成换目录（说明文字、规则文件
/// 都再来一遍），POSIX 上 `/work/` 也是。
#[tokio::test]
async fn the_same_directory_is_judged_by_its_path_key() {
    use rust_cli_kernel_api::manage::DirectoryOutcome;
    use rust_cli_kernel_api::session_api::{Ack, Command};

    use crate::test_support::envelope;

    let unchanged = |root: &'static str, target: &'static str| async move {
        let mut setup = Setup::new(Arc::new(ScriptedModel::new(Vec::new())));
        setup.set_env(Arc::new(CwdEnv {
            inner: env(),
            cwd: Mutex::default(),
            refuse: false,
        }));
        setup.root = root;
        let built = setup.build().await;
        let command = Command::ChangeDirectory {
            root: EnvPath::new(target),
        };
        let Ack::Accepted { result } = built.session.send(envelope("c1", command)).await else {
            panic!("refused");
        };
        serde_json::from_value::<DirectoryOutcome>(result).unwrap() == DirectoryOutcome::Unchanged
    };
    assert!(unchanged(r"C:\Work", r"c:\work").await);
    assert!(unchanged(r"C:\Work", r"C:\Work\").await);
    assert!(unchanged("/work", "/work/").await);
    // POSIX 区分大小写：是另一个目录。
    assert!(!unchanged("/work", "/WORK").await);
}
