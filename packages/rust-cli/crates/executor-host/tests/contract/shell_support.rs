//! shell 命令的夹具（只在 Unix：要 `/bin/bash`）。

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use futures_util::StreamExt;
use rust_cli_env_local::{LocalEnvironment, ShellConfig};
use rust_cli_kernel_api::env::{CommandEvent, Outcome, StartCommand, WorkspaceRef};
use rust_cli_kernel_api::env_shell::CwdScope;
use rust_cli_kernel_api::ids::{OperationId, SessionId, ShellTaskId};

use crate::support::{Kind, Machine, context, env_path, machine, workspace};

/// shell 命令的夹具：`<临时目录>/work` 是工作区，输出与账本在旁边的目录。
pub struct Shell {
    _dir: tempfile::TempDir,
    pub root: PathBuf,
    pub machine: Machine,
}

pub async fn shell(kind: Kind, env: Vec<(String, String)>) -> Shell {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let root = base.join("work");
    std::fs::create_dir_all(&root).unwrap();
    let local = LocalEnvironment::new(None).with_shell(ShellConfig {
        scratch: base.join("scratch"),
        ledger: base.join("ledger"),
        bash: Some(PathBuf::from("/bin/bash")),
        bwrap: None,
        embedded: None,
        env: env.into(),
    });
    Shell {
        _dir: dir,
        root,
        machine: machine(kind, local).await,
    }
}

static NEXT_TASK: AtomicU64 = AtomicU64::new(1);

impl Shell {
    pub fn workspace(&self) -> WorkspaceRef {
        workspace(&self.root)
    }

    pub fn request(&self, command: &str, scope: CwdScope) -> StartCommand {
        StartCommand {
            command: command.to_owned(),
            cwd: env_path(&self.root),
            timeout_ms: Some(10_000),
            task: ShellTaskId::new(format!("b{}", NEXT_TASK.fetch_add(1, Ordering::Relaxed))),
            shell: Default::default(),
            cwd_scope: scope,
            background: None,
            sandbox: None,
            hook: None,
            on_timeout: None,
            socket: None,
            env: Default::default(),
        }
    }

    pub async fn start(&self, operation: &str, start: StartCommand) {
        self.machine
            .commands()
            .start_command(&self.workspace(), context(operation), start)
            .await
            .unwrap();
    }

    /// 跑到终态：合并的输出与终态事件。
    pub async fn run(&self, operation: &str, start: StartCommand) -> Ran {
        self.start(operation, start).await;
        self.collect(operation).await
    }

    pub async fn collect(&self, operation: &str) -> Ran {
        let mut events = self
            .machine
            .commands()
            .watch_command(OperationId::new(operation), 0);
        assert!(matches!(
            events.next().await,
            Some(Ok(CommandEvent::Started { .. }))
        ));
        let mut output = Vec::new();
        while let Some(event) = events.next().await {
            match event.unwrap() {
                CommandEvent::Output { data, .. } => output.extend(data),
                finished @ CommandEvent::Finished { .. } => {
                    return Ran {
                        output: String::from_utf8(output).unwrap(),
                        finished,
                    };
                }
                CommandEvent::Started { .. } => panic!("started twice"),
                CommandEvent::Backgrounded => panic!("moved to the background"),
            }
        }
        panic!("no terminal event");
    }

    pub async fn background(&self) -> Vec<ShellTaskId> {
        self.machine
            .shell()
            .shell_state(&self.workspace(), &SessionId::new("s1"))
            .await
            .unwrap()
            .background
    }
}

pub struct Ran {
    pub output: String,
    pub finished: CommandEvent,
}

pub fn exit_of(event: &CommandEvent) -> (Outcome, Option<i32>, bool) {
    let CommandEvent::Finished {
        outcome,
        exit_code,
        timed_out,
        ..
    } = event
    else {
        panic!("not finished");
    };
    (outcome.clone(), *exit_code, *timed_out)
}
