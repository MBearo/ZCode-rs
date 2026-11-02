//! kernel → 线上 → kernel 的往返（F128，审查 X1）：客户端（env-remote）把端口的参数换成线上的请求，服务端（exec-server）
//! 换回 kernel-api 的类型交给替身；替身的结果再经服务端、客户端换回来。每个字段都填非缺省的值，往返后必须相等：换算漏了
//! 字段、或把两个字段对调时这里失败。不上线的字段单独核对（转后台的原因）。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::{self, BoxStream};
use rust_cli_kernel_api::env::{
    CommandEvent, CommandKind, EnvError, EnvFacts, EnvPath, FileContent, FileVersion,
    OperationContext, Outcome, OutputStream, ReadFile, StartCommand, WorkspaceRef, WriteFile,
    WritePrecondition, WriteReceipt,
};
use rust_cli_kernel_api::env_command::BackgroundReason;
use rust_cli_kernel_api::env_files::{
    FileWritten, GlobFiles, GlobMatches, GrepCount, GrepFileHit, GrepFileLines, GrepLine,
    GrepLineKind, GrepMode, GrepOutcome, GrepSearch, GrepSubmatch, PathFacts, PathStyle, PutFile,
    ReadText, TextRead, TextSlice,
};
use rust_cli_kernel_api::env_pdf::{PdfRender, RenderPdfPages};
use rust_cli_kernel_api::env_rewind::{
    RestoreAction, RestoreFile, RestoredFile, RewindConflict, RewindPlan, RewindPrepare,
    RewindStatus,
};
use rust_cli_kernel_api::env_shell::{
    BackgroundCommand, BackgroundInfo, BackgroundKind, BackgroundStatus, BackgroundStop,
    BackgroundTask, CwdScope, FilesystemPolicy, HookIo, MonitorSocket, NetworkPolicy,
    SandboxPolicy, ShellKind, ShellNotice, ShellState, TimeoutBackground,
};
use rust_cli_kernel_api::ids::{OperationId, SessionId, ShellTaskId, ToolUseId};
use rust_cli_kernel_api::machine::{
    Background, Checkpoints, CommandEvents, Commands, EnvInfo, Files, Git, MachineEnv, Pdf, Search,
    Shell,
};
use rust_cli_kernel_api::mcp::NoMcpTransport;

use crate::support::{Machine, context, loopback};

/// 服务端收到的参数（已经换回 kernel-api 的类型）。
#[derive(Debug, Clone, PartialEq)]
enum Seen {
    Start(WorkspaceRef, OperationContext, Box<StartCommand>),
    Glob(GlobFiles),
    Grep(GrepSearch),
    ReadText(ReadText),
    Put(OperationContext, PutFile),
    Rewind(OperationContext, RewindPrepare),
    Render(RenderPdfPages),
    Stop(OperationContext, BackgroundTask),
}

/// 替身：记下收到的参数，回固定的、每个字段都有值的结果。
#[derive(Default)]
struct Echo {
    seen: Mutex<Vec<Seen>>,
}

impl Echo {
    fn saw(&self, seen: Seen) {
        self.seen.lock().unwrap().push(seen);
    }
}

fn path(text: &str) -> EnvPath {
    EnvPath::new(text)
}

fn workspace() -> WorkspaceRef {
    WorkspaceRef {
        environment: rust_cli_kernel_api::ids::EnvironmentId::new("box"),
        root: path("/srv/work"),
    }
}

fn background(description: &str, kind: BackgroundKind) -> BackgroundCommand {
    BackgroundCommand {
        description: description.to_owned(),
        tool_use: Some(ToolUseId::new("toolu_1")),
        kind,
    }
}

fn facts() -> EnvFacts {
    EnvFacts {
        cwd: path("/srv/work"),
        platform: "linux".to_owned(),
        os_version: "Linux 6.1".to_owned(),
        shell: "zsh".to_owned(),
        is_git_repo: true,
        home: Some(path("/home/dev")),
        uid: Some(1000),
    }
}

fn slice() -> TextSlice {
    TextSlice {
        content: "one\ntwo".to_owned(),
        line_count: 2,
        total_lines: 9,
        total_bytes: 40,
        read_bytes: 7,
        mtime_ms: 1_700_000_000_123,
        version: FileVersion::new("v7"),
    }
}

fn grep_outcome(mode: &GrepMode) -> GrepOutcome {
    match mode {
        GrepMode::FilesWithMatches => GrepOutcome::Files {
            files: vec![GrepFileHit {
                path: path("/srv/work/a.rs"),
                mtime_ms: 11,
            }],
        },
        GrepMode::Count => GrepOutcome::Counts {
            counts: vec![GrepCount {
                path: path("/srv/work/a.rs"),
                count: 3,
            }],
        },
        GrepMode::Content { .. } => GrepOutcome::Content {
            files: vec![GrepFileLines {
                path: path("/srv/work/a.rs"),
                lines: vec![
                    GrepLine {
                        kind: GrepLineKind::Context,
                        line_number: 4,
                        text: "before".to_owned(),
                        submatches: Vec::new(),
                    },
                    GrepLine {
                        kind: GrepLineKind::Match,
                        line_number: 5,
                        text: "let needle = 1;".to_owned(),
                        submatches: vec![GrepSubmatch {
                            start: 4,
                            text: "needle".to_owned(),
                        }],
                    },
                ],
                binary_offset: Some(99),
            }],
        },
    }
}

fn conflict() -> RewindConflict {
    RewindConflict {
        path: path("/srv/work/c.txt"),
        current: Some("hash".to_owned()),
        message: Some("changed".to_owned()),
    }
}

fn notice() -> ShellNotice {
    ShellNotice {
        seq: 4,
        operation: OperationId::new("op_bg"),
        task: ShellTaskId::new("b9"),
        tool_use: Some(ToolUseId::new("toolu_9")),
        description: "watch the build".to_owned(),
        output: path("/tmp/b9.output"),
        outcome: Outcome::Failed {
            code: Some("exit".to_owned()),
            message: "exit 2".to_owned(),
        },
        exit_code: Some(2),
        kind: BackgroundKind::Monitor,
        output_bytes: 77,
    }
}

fn events() -> Vec<CommandEvent> {
    vec![
        CommandEvent::Started {
            output: path("/tmp/b1.output"),
        },
        CommandEvent::Output {
            stream: OutputStream::Stdout,
            data: b"out".to_vec(),
            cursor: 3,
        },
        CommandEvent::Backgrounded,
        CommandEvent::Finished {
            outcome: Outcome::Cancelled,
            exit_code: Some(137),
            timed_out: true,
            cwd_reset: Some(path("/srv/work")),
            output_bytes: 3,
            output_limit: Some("Command killed: output file exceeded 5GB".to_owned()),
        },
    ]
}

#[async_trait]
impl EnvInfo for Echo {
    async fn facts(&self, _: &WorkspaceRef) -> Result<EnvFacts, EnvError> {
        Ok(facts())
    }

    async fn path_facts(&self, _: &WorkspaceRef) -> Result<PathFacts, EnvError> {
        Ok(PathFacts {
            home: Some(path("C:\\Users\\dev")),
            style: PathStyle::Windows,
        })
    }
}

#[async_trait]
impl Files for Echo {
    async fn read_file(&self, _: &WorkspaceRef, _: ReadFile) -> Result<FileContent, EnvError> {
        Err(EnvError::Unsupported {
            what: "read_file".to_owned(),
        })
    }

    async fn write_file(
        &self,
        _: &WorkspaceRef,
        _: OperationContext,
        _: WriteFile,
    ) -> Result<WriteReceipt, EnvError> {
        Err(EnvError::Unsupported {
            what: "write_file".to_owned(),
        })
    }

    async fn read_text(&self, _: &WorkspaceRef, request: ReadText) -> Result<TextRead, EnvError> {
        self.saw(Seen::ReadText(request));
        Ok(TextRead::Text(slice()))
    }

    async fn put_file(
        &self,
        _: &WorkspaceRef,
        context: OperationContext,
        request: PutFile,
    ) -> Result<FileWritten, EnvError> {
        self.saw(Seen::Put(context, request));
        Ok(FileWritten {
            version: FileVersion::new("v8"),
            mtime_ms: 42,
            created: true,
        })
    }
}

#[async_trait]
impl Search for Echo {
    async fn glob(&self, _: &WorkspaceRef, request: GlobFiles) -> Result<GlobMatches, EnvError> {
        self.saw(Seen::Glob(request));
        Ok(GlobMatches {
            files: vec![path("/srv/work/b.rs"), path("/srv/work/a.rs")],
            total: 12,
        })
    }

    async fn grep(&self, _: &WorkspaceRef, request: GrepSearch) -> Result<GrepOutcome, EnvError> {
        let outcome = grep_outcome(&request.mode);
        self.saw(Seen::Grep(request));
        Ok(outcome)
    }
}

#[async_trait]
impl Commands for Echo {
    async fn start_command(
        &self,
        workspace: &WorkspaceRef,
        context: OperationContext,
        request: StartCommand,
    ) -> Result<CommandEvents, EnvError> {
        self.saw(Seen::Start(workspace.clone(), context, Box::new(request)));
        let mut events: Vec<Result<CommandEvent, EnvError>> =
            events().into_iter().map(Ok).collect();
        // 转后台的原因不上线（远端不支持按要求转后台）：到客户端是 `Backgrounded`。
        events.insert(
            2,
            Ok(CommandEvent::BackgroundedBy {
                reason: BackgroundReason::User,
            }),
        );
        Ok(stream::iter(events).boxed())
    }

    fn watch_command(
        &self,
        _: OperationId,
        _: u64,
    ) -> BoxStream<'static, Result<CommandEvent, EnvError>> {
        stream::empty().boxed()
    }

    async fn cancel(&self, _: OperationId) -> Result<(), EnvError> {
        Ok(())
    }

    async fn close_session(&self, _: &SessionId, _: bool) -> Result<(), EnvError> {
        Ok(())
    }
}

#[async_trait]
impl Shell for Echo {
    async fn shell_state(&self, _: &WorkspaceRef, _: &SessionId) -> Result<ShellState, EnvError> {
        Ok(ShellState {
            cwd: path("/srv/work/sub"),
            outputs: Some(path("/tmp/s1/tasks")),
            background: vec![ShellTaskId::new("b1"), ShellTaskId::new("b2")],
        })
    }

    fn shell_notices(
        &self,
        _: &SessionId,
        _: u64,
    ) -> BoxStream<'static, Result<ShellNotice, EnvError>> {
        stream::iter([Ok(notice())]).boxed()
    }
}

#[async_trait]
impl Background for Echo {
    async fn background_command(
        &self,
        _: BackgroundTask,
    ) -> Result<Option<BackgroundInfo>, EnvError> {
        Ok(Some(BackgroundInfo {
            command: "tail -f log".to_owned(),
            status: BackgroundStatus::Killed,
            kind: BackgroundKind::Monitor,
            description: "follow the log".to_owned(),
        }))
    }

    async fn stop_background_command(
        &self,
        context: OperationContext,
        request: BackgroundTask,
    ) -> Result<BackgroundStop, EnvError> {
        self.saw(Seen::Stop(context, request));
        Ok(BackgroundStop::NotRunning {
            status: BackgroundStatus::Completed,
        })
    }
}

#[async_trait]
impl Checkpoints for Echo {
    async fn rewind_prepare(
        &self,
        _: &WorkspaceRef,
        context: OperationContext,
        request: RewindPrepare,
    ) -> Result<RewindPlan, EnvError> {
        self.saw(Seen::Rewind(context, request));
        Ok(RewindPlan::Conflicts {
            files: vec![conflict()],
        })
    }

    async fn rewind_apply(&self, _: &OperationId) -> Result<RewindStatus, EnvError> {
        Ok(RewindStatus::Applied {
            files: vec![
                RestoredFile {
                    path: path("/srv/work/a.txt"),
                    action: RestoreAction::Restore,
                },
                RestoredFile {
                    path: path("/srv/work/new.txt"),
                    action: RestoreAction::Delete,
                },
            ],
        })
    }
}

#[async_trait]
impl Pdf for Echo {
    async fn render_pdf_pages(
        &self,
        _: &WorkspaceRef,
        request: RenderPdfPages,
    ) -> Result<PdfRender, EnvError> {
        self.saw(Seen::Render(request));
        Ok(PdfRender::Rendered {
            pages: vec![b"page-one".to_vec(), b"page-two".to_vec()],
        })
    }
}

impl Git for Echo {}

async fn setup() -> (Arc<Echo>, Machine) {
    let echo = Arc::new(Echo::default());
    let served = MachineEnv::new(echo.clone(), Arc::new(NoMcpTransport));
    (echo, loopback(served).await)
}

/// 每种命令一个（线上是四个可选字段，两边各自拼回种类）。
fn start_commands() -> Vec<StartCommand> {
    let plain = StartCommand {
        timeout_ms: Some(120_000),
        shell: ShellKind::PowerShell,
        cwd_scope: CwdScope::Session {
            roots: vec![path("/srv/work"), path("/srv/other")],
        },
        sandbox: Some(SandboxPolicy {
            filesystem: FilesystemPolicy {
                allow_write: vec![path("/srv/work")],
                deny_write: vec![path("/srv/work/.git")],
                deny_read: vec![path("/home/dev/.ssh")],
            },
            network: NetworkPolicy::Domains {
                allowed: vec!["example.com".to_owned()],
                denied: vec!["evil.example.com".to_owned()],
            },
            required: true,
        }),
        kind: CommandKind::Foreground {
            on_timeout: Some(TimeoutBackground {
                command: background("moved", BackgroundKind::Bash),
                deadline_ms: Some(600_000),
            }),
        },
        ..StartCommand::new(
            "make test".to_owned(),
            path("/srv/work/sub"),
            ShellTaskId::new("b1"),
        )
    };
    [
        CommandKind::Foreground { on_timeout: None },
        CommandKind::Background {
            background: background("run the tests", BackgroundKind::Monitor),
        },
        CommandKind::Hook {
            hook: HookIo {
                stdin: "{\"hook\":1}\n".to_owned(),
                env: BTreeMap::from([("HOOK_VAR".to_owned(), "1".to_owned())]),
            },
        },
        CommandKind::Socket {
            background: background("watch the feed", BackgroundKind::Monitor),
            socket: MonitorSocket {
                url: "wss://example.com/feed".to_owned(),
                protocols: vec!["v1".to_owned()],
            },
        },
    ]
    .into_iter()
    .map(|kind| StartCommand {
        kind,
        ..plain.clone()
    })
    .chain([plain.clone()])
    .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn every_kind_of_command_and_its_events_survive_the_wire() {
    let (echo, machine) = setup().await;
    let mut sent = Vec::new();
    for (index, request) in start_commands().into_iter().enumerate() {
        let ctx = context(&format!("op_start_{index}"));
        let received: Vec<CommandEvent> = machine
            .commands()
            .start_command(&workspace(), ctx.clone(), request.clone())
            .await
            .unwrap()
            .map(Result::unwrap)
            .collect()
            .await;
        // 转后台的原因不上线：`BackgroundedBy` 到客户端是 `Backgrounded`。
        let mut expected = events();
        expected.insert(2, CommandEvent::Backgrounded);
        assert_eq!(received, expected);
        sent.push(Seen::Start(workspace(), ctx, Box::new(request)));
    }
    assert_eq!(echo.seen.lock().unwrap().clone(), sent);
}

#[tokio::test(flavor = "multi_thread")]
async fn search_and_read_requests_and_results_survive_the_wire() {
    let (echo, machine) = setup().await;
    let glob = GlobFiles {
        root: path("/srv/work"),
        pattern: "**/*.rs".to_owned(),
        limit: 50,
        hidden: true,
        respect_ignore: false,
    };
    let matches = machine
        .search()
        .glob(&workspace(), glob.clone())
        .await
        .unwrap();
    assert_eq!(
        matches,
        GlobMatches {
            files: vec![path("/srv/work/b.rs"), path("/srv/work/a.rs")],
            total: 12,
        }
    );
    let mut sent = vec![Seen::Glob(glob)];
    for mode in [
        GrepMode::FilesWithMatches,
        GrepMode::Count,
        GrepMode::Content {
            before: 1,
            after: 2,
        },
    ] {
        let grep = GrepSearch {
            root: path("/srv/work"),
            pattern: "needle".to_owned(),
            case_insensitive: true,
            multiline: true,
            globs: vec!["*.rs".to_owned(), "!target".to_owned()],
            file_type: Some("rust".to_owned()),
            hidden: true,
            mode: mode.clone(),
        };
        let outcome = machine
            .search()
            .grep(&workspace(), grep.clone())
            .await
            .unwrap();
        assert_eq!(outcome, grep_outcome(&mode));
        sent.push(Seen::Grep(grep));
    }
    let read = ReadText {
        path: path("/srv/work/a.txt"),
        start_line: 3,
        max_lines: Some(40),
        max_bytes: Some(4096),
    };
    let text = machine
        .files()
        .read_text(&workspace(), read.clone())
        .await
        .unwrap();
    assert_eq!(text, TextRead::Text(slice()));
    sent.push(Seen::ReadText(read));
    assert_eq!(machine.info().facts(&workspace()).await.unwrap(), facts());
    assert_eq!(
        machine.info().path_facts(&workspace()).await.unwrap(),
        PathFacts {
            home: Some(path("C:\\Users\\dev")),
            style: PathStyle::Windows,
        }
    );
    assert_eq!(echo.seen.lock().unwrap().clone(), sent);
}

#[tokio::test(flavor = "multi_thread")]
async fn writes_rewinds_and_background_survive_the_wire() {
    let (echo, machine) = setup().await;
    let ctx = context("op_write");
    let put = PutFile {
        path: path("/srv/work/new/a.txt"),
        bytes: b"hello".to_vec(),
        precondition: WritePrecondition::Version(FileVersion::new("v6")),
        create_parents: true,
    };
    let written = machine
        .files()
        .put_file(&workspace(), ctx.clone(), put.clone())
        .await
        .unwrap();
    assert_eq!(
        written,
        FileWritten {
            version: FileVersion::new("v8"),
            mtime_ms: 42,
            created: true,
        }
    );
    let prepare = RewindPrepare {
        files: vec![
            RestoreFile {
                path: path("/srv/work/a.txt"),
                expected: Some("after".to_owned()),
                restore: Some(b"before".to_vec()),
            },
            RestoreFile {
                path: path("/srv/work/new.txt"),
                expected: None,
                restore: None,
            },
        ],
    };
    let plan = machine
        .checkpoints()
        .rewind_prepare(&workspace(), ctx.clone(), prepare.clone())
        .await
        .unwrap();
    assert_eq!(
        plan,
        RewindPlan::Conflicts {
            files: vec![conflict()],
        }
    );
    let token = OperationId::new("op_rewind");
    let applied = machine.checkpoints().rewind_apply(&token).await.unwrap();
    assert!(matches!(applied, RewindStatus::Applied { files } if files.len() == 2));
    let render = RenderPdfPages {
        path: path("/srv/work/doc.pdf"),
        first_page: 2,
        last_page: Some(3),
    };
    let pages = machine
        .pdf()
        .render_pdf_pages(&workspace(), render.clone())
        .await
        .unwrap();
    assert_eq!(
        pages,
        PdfRender::Rendered {
            pages: vec![b"page-one".to_vec(), b"page-two".to_vec()],
        }
    );
    let task = BackgroundTask {
        session: SessionId::new("s1"),
        task: ShellTaskId::new("b1"),
    };
    let stop = machine
        .background()
        .stop_background_command(ctx.clone(), task.clone())
        .await
        .unwrap();
    assert_eq!(
        stop,
        BackgroundStop::NotRunning {
            status: BackgroundStatus::Completed,
        }
    );
    let info = machine
        .background()
        .background_command(task.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            info.command.as_str(),
            info.status,
            info.kind,
            info.description.as_str()
        ),
        (
            "tail -f log",
            BackgroundStatus::Killed,
            BackgroundKind::Monitor,
            "follow the log"
        )
    );
    let state = machine
        .shell()
        .shell_state(&workspace(), &SessionId::new("s1"))
        .await
        .unwrap();
    assert_eq!(
        state,
        ShellState {
            cwd: path("/srv/work/sub"),
            outputs: Some(path("/tmp/s1/tasks")),
            background: vec![ShellTaskId::new("b1"), ShellTaskId::new("b2")],
        }
    );
    let notices: Vec<ShellNotice> = machine
        .shell()
        .shell_notices(&SessionId::new("s1"), 0)
        .take(1)
        .map(Result::unwrap)
        .collect()
        .await;
    assert_eq!(notices, [notice()]);
    assert_eq!(
        echo.seen.lock().unwrap().clone(),
        [
            Seen::Put(ctx.clone(), put),
            Seen::Rewind(ctx.clone(), prepare),
            Seen::Render(render),
            Seen::Stop(ctx, task),
        ]
    );
}
