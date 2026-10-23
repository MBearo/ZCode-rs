//! 本机 shell 命令（docs/specs/rust-cli/shell-permissions.md §2）。
//!
//! 每条命令一个 [`Running`] 记录，按 operationId 登记（同一个 ID、同样的参数重放时不再执行，
//! 参数不同时拒绝）。进程经 `rust-cli-proc` 的账本启动：Unix 上自成会话与进程组，Windows 上放进
//! 自己的 Job，取消、超时与结束时整棵树一起回收。stdout 与 stderr 写进同一个输出文件（合并的
//! fd），[`watch`] 从文件按游标读。监督任务在执行环境自己的任务作用域里运行，后台命令
//! 因此不随工具调用结束；它的终态作为 [`ShellNotice`] 记在会话里，ack 之前一直保留。

mod embedded;
pub(crate) mod environ;
mod mounts;
mod notices;
mod sandbox;
mod stop;
mod supervise;
mod watch;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures_util::stream::BoxStream;
use rust_cli_base::TaskScope;
use rust_cli_base::secret::EnvSnapshot;
use rust_cli_kernel_api::env::{
    CommandEvent, EnvError, EnvPath, OperationContext, Origin, Outcome, StartCommand, WorkspaceRef,
};
use rust_cli_kernel_api::env_shell::{
    BackgroundStop, BackgroundTask, CwdScope, ShellKind, ShellNotice, ShellState,
};
use rust_cli_kernel_api::ids::{OperationId, SessionId, ShellTaskId};
use rust_cli_proc::ledger::Ledger;
use tokio::sync::watch as signal;
use tokio_util::sync::CancellationToken;

pub use embedded::EmbeddedSearch;
pub(crate) use sandbox::Backend;

/// 本机 shell 的配置，由入口 crate 给出（只有它读进程环境）。
#[derive(Debug, Clone, Default)]
pub struct ShellConfig {
    /// 输出文件与 cwd 文件的根目录：`<scratch>/<会话>/tasks/<任务>.output`。必须是本用户私有的目录
    /// （每次启动命令前经 `rust_cli_proc::private::ensure` 核对）；入口给的是 `<tmp>/zcode-<uid>`。
    pub scratch: PathBuf,
    /// 进程树账本目录（`rust-cli-proc`，同样核对）；所在进程被强杀后，下一次启动时清扫。入口给的与
    /// `scratch` 相同。
    pub ledger: PathBuf,
    /// 跑 Bash 工具命令的 shell（入口找的 bash 或 zsh，shell-permissions.md §2.5；字段名沿用）。
    /// None 时 Bash 工具的命令返回错误；hook 命令不用它（Unix 上固定 `/bin/bash`）。
    pub bash: Option<PathBuf>,
    /// Linux 上的 bwrap；没有时 sandbox 不可用。
    pub bwrap: Option<PathBuf>,
    /// 启动时捕获的进程环境，按白名单投影给用户命令（`environ::project`）。`Debug` 只列变量名。
    pub env: EnvSnapshot,
    /// 内嵌的 rg / bfs / ugrep（docs/specs/rust-cli/embedded-search.md §8，F53）：有值时 Bash 工具的每条命令前加定义
    /// `find` / `grep` / `rg` 函数的 prelude，环境里设 `ZCODE_EXECPATH`；hook 命令不加。None 时与以前相同。
    pub embedded: Option<EmbeddedSearch>,
}

/// 入口找不到 shell 时 Bash 工具命令的错误（docs/specs/rust-cli/shell-permissions.md §2.1）。Unix 上是固定的原文，
/// 产品名写 `ZCode`（修复线 F51，用户决定 2026-10-22：发给模型的文字里只用本产品的说法；F44 时逐字保留）；
/// Windows 上是没有 Git Bash（那里不提供 Bash 工具）。
///
/// 缺陷修复（F44）：以前 Unix 上固定 `/bin/bash`，没有它的镜像里命令启动失败，只有 `No such file or directory`。
fn no_shell() -> EnvError {
    if cfg!(windows) {
        return EnvError::Unsupported {
            what: "Bash commands need Git Bash on Windows (set its path)".to_owned(),
        };
    }
    EnvError::Io {
        message: "No suitable shell found. ZCode requires a Posix shell environment. Please ensure you have a valid shell installed and the SHELL environment variable set.".to_owned(),
    }
}

/// hook 命令用的 shell：Unix 上固定 `/bin/bash`，不随 Bash 工具找到的 shell（hooks.md D4）；
/// Windows 上是 Git Bash（入口找到的就是它）。
fn hook_shell(config: &ShellConfig) -> Result<PathBuf, EnvError> {
    if cfg!(windows) {
        return config.bash.clone().ok_or_else(no_shell);
    }
    Ok(PathBuf::from("/bin/bash"))
}

/// 输出文件的上限（docs/specs/rust-cli/shell-permissions.md §2.2“输出上限”，审查 R2-18）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OutputLimit {
    /// 输出文件（hook 命令是 stdout 与 stderr 两个文件之和）超过这么多字节时结束命令。
    pub bytes: u64,
    /// 说明文字里的写法（`Command killed: output file exceeded <label>`）。
    pub label: &'static str,
    /// 多久查一次大小。
    pub every: Duration,
}

/// 输出的 size watchdog：上限 5 GB（显示为 "5GB"），每 5 s 查一次。
pub(crate) const OUTPUT_LIMIT: OutputLimit = OutputLimit {
    bytes: 5 * 1024 * 1024 * 1024,
    label: "5GB",
    every: Duration::from_secs(5),
};

/// 命令的终态（[`CommandEvent::Finished`] 的内容）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Finish {
    pub outcome: Outcome,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub cwd_reset: Option<EnvPath>,
    pub output_bytes: u64,
    /// 因输出超过上限被结束时的说明。
    pub output_limit: Option<String>,
}

impl Finish {
    pub(crate) fn event(&self) -> CommandEvent {
        CommandEvent::Finished {
            outcome: self.outcome.clone(),
            exit_code: self.exit_code,
            timed_out: self.timed_out,
            cwd_reset: self.cwd_reset.clone(),
            output_bytes: self.output_bytes,
            output_limit: self.output_limit.clone(),
        }
    }
}

/// 一条已登记的命令。
pub(crate) struct Running {
    fingerprint: String,
    session: SessionId,
    /// 命令原文（TaskStop 结果的 `command`）。
    pub command: String,
    pub output: PathBuf,
    /// hook 命令的 stderr 文件（与 stdout 分开，终态前送达，hooks.md §5.1）。
    pub errors: Option<PathBuf>,
    pub cancel: CancellationToken,
    /// `None` 表示还在运行。
    pub finished: signal::Sender<Option<Finish>>,
    /// 前台命令到了超时、已转到后台（`StartCommand::on_timeout`，shell-permissions.md §1.2.1）。
    pub backgrounded: signal::Sender<bool>,
    /// 开始的时刻（Shell 详情的 Runtime，docs/specs/rust-cli/tui-panels.md §3.3）。
    pub started: std::time::Instant,
}

#[derive(Default)]
pub(crate) struct SessionShell {
    pub cwd: Option<PathBuf>,
    /// 还没有 ack 的后台通知，按 seq 升序。
    pub notices: Vec<ShellNotice>,
    /// 还在跑的后台命令（任务 → 操作）。与 `notices` 在同一把锁里变化：命令结束时移出这里、同时记
    /// 通知，所以 [`ShellState::background`] 不会出现"已结束但两边都没有"的空档。
    pub background: BTreeMap<ShellTaskId, OperationId>,
    /// TaskStop 停掉、进程还没收尾的命令：收尾时不记通知（docs/specs/rust-cli/shell-permissions.md §2.6）。
    pub stopped: BTreeSet<ShellTaskId>,
    pub next_seq: u64,
    pub acked: u64,
}

#[derive(Default)]
pub(crate) struct State {
    pub commands: HashMap<OperationId, Arc<Running>>,
    pub sessions: HashMap<SessionId, SessionShell>,
    /// 停止操作的结果，按 operationId 重放（README §6 规则 3）。
    pub stops: HashMap<OperationId, (BackgroundTask, BackgroundStop)>,
    /// 正在跑的 Linux sandbox 与 bwrap 在宿主上建的挂载点（命令结束后清理）。
    pub mounts: mounts::Mounts,
}

/// 监督任务与 [`Shells`] 共享的状态。锁只在同步代码里短暂持有。
pub(crate) struct Shared {
    state: Mutex<State>,
    /// 任何会话有新通知时递增，通知流据此醒来再查自己的会话。
    pub notices: signal::Sender<u64>,
}

impl Shared {
    pub(crate) fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// 本机 shell 的运行时。释放时它的任务作用域中止监督任务，进程树随 `Tracked` 一起回收。
pub(crate) struct Shells {
    config: ShellConfig,
    backend: Backend,
    ledger: Ledger,
    scope: TaskScope,
    shared: Arc<Shared>,
    /// 输出文件的上限（生产是 [`OUTPUT_LIMIT`]；测试换成小的）。
    limit: OutputLimit,
}

fn session_of(context: &OperationContext) -> SessionId {
    match &context.origin {
        Origin::Agent { session, .. } => session.clone(),
        Origin::User { session } => session.clone().unwrap_or_else(|| SessionId::new("user")),
    }
}

impl Shells {
    /// 本用户私有的临时目录（PDF 取页的输出目录也在这里，tools-files.md §1.5）。
    pub(crate) fn scratch(&self) -> &std::path::Path {
        &self.config.scratch
    }

    /// 某个会话的输出目录 `<scratch>/<会话>/tasks`（F55：以前在启动与查询两处各拼一次）。
    fn task_outputs(&self, session: &SessionId) -> PathBuf {
        self.config.scratch.join(session.as_str()).join("tasks")
    }

    pub(crate) fn new(config: ShellConfig) -> Self {
        let backend = if cfg!(target_os = "macos") {
            Backend::MacOs
        } else if let (true, Some(bwrap)) = (cfg!(target_os = "linux"), config.bwrap.clone()) {
            Backend::Linux { bwrap }
        } else {
            Backend::Unavailable
        };
        let scope = TaskScope::root("env-local/shell", |panic| {
            tracing::error!(task = panic.task, message = %panic.message, "shell supervisor panicked");
        });
        Self {
            ledger: Ledger::open(config.ledger.clone()),
            config,
            backend,
            scope,
            shared: Arc::new(Shared {
                state: Mutex::default(),
                notices: signal::channel(0).0,
            }),
            limit: OUTPUT_LIMIT,
        }
    }

    /// 测试用小的上限与间隔（5 GB / 5 s 在测试里不可行）。只有 Unix 的测试用（F66：Windows 上是死代码）。
    #[cfg(all(test, unix))]
    pub(crate) fn set_output_limit(&mut self, limit: OutputLimit) {
        self.limit = limit;
    }

    fn command(&self, operation: &OperationId) -> Option<Arc<Running>> {
        self.shared.state().commands.get(operation).cloned()
    }

    pub(crate) async fn start(
        &self,
        context: OperationContext,
        request: StartCommand,
    ) -> Result<(), EnvError> {
        let fingerprint = serde_json::to_string(&request).map_err(|error| EnvError::Io {
            message: error.to_string(),
        })?;
        if let Some(known) = self.command(&context.operation) {
            return if known.fingerprint == fingerprint {
                Ok(())
            } else {
                Err(EnvError::OperationConflict {
                    operation: context.operation,
                })
            };
        }
        if request.shell == ShellKind::PowerShell {
            return Err(EnvError::Unsupported {
                what: "PowerShell commands in the local environment".to_owned(),
            });
        }
        let bash = if request.hook.is_some() {
            hook_shell(&self.config)?
        } else {
            self.config.bash.clone().ok_or_else(no_shell)?
        };
        let session = session_of(&context);
        let origin = PathBuf::from(request.cwd.as_str());
        let cwd = match &request.cwd_scope {
            CwdScope::Pinned => origin.clone(),
            CwdScope::Session { .. } => self
                .shared
                .state()
                .sessions
                .get(&session)
                .and_then(|shell| shell.cwd.clone())
                .unwrap_or_else(|| origin.clone()),
        };
        // 缺陷修复 R2-1：输出文件所在的根目录先核对是本用户私有的（0700、属主是本 uid、不是符号链接），
        // 不是时命令报错、不启动；原来直接在共享临时目录里按默认权限建。
        rust_cli_proc::private::ensure(&self.config.scratch)
            .await
            .map_err(|error| EnvError::Io {
                message: error.to_string(),
            })?;
        let dir = self.task_outputs(&session);
        let shell_name = bash.to_string_lossy().into_owned();
        let mut env = environ::project(&self.config.env, Some(&shell_name));
        if let Some(hook) = &request.hook {
            env.extend(hook.env.clone());
        }
        // 内嵌搜索工具的 prelude 只给 Bash 工具与 `!` 命令（hook 命令不加 prelude、不设本进程路径的变量）；
        // 环境的白名单投影不变，这个变量单独加。
        let prelude = match (&request.hook, &self.config.embedded) {
            (None, Some(search)) => {
                env.insert(
                    embedded::EXECPATH.to_owned(),
                    search.executable.to_string_lossy().into_owned(),
                );
                Some(embedded::prelude(search, cfg!(windows)))
            }
            _ => None,
        };
        let plan = supervise::Plan {
            output: dir.join(format!("{}.output", request.task)),
            errors: request
                .hook
                .as_ref()
                .map(|_| dir.join(format!("{}.stderr", request.task))),
            cwd_file: dir.join(format!("{}.cwd", request.task)),
            dir,
            cwd,
            origin,
            env,
            bash,
            prelude,
        };
        let (launched, mounted) =
            supervise::spawn(&self.ledger, &self.backend, &self.shared, &plan, &request).await?;
        let running = Arc::new(Running {
            fingerprint,
            session: session.clone(),
            command: request.command.clone(),
            output: plan.output.clone(),
            errors: plan.errors.clone(),
            cancel: CancellationToken::new(),
            finished: signal::channel(None).0,
            backgrounded: signal::channel(false).0,
            started: std::time::Instant::now(),
        });
        {
            let mut state = self.shared.state();
            state
                .commands
                .insert(context.operation.clone(), running.clone());
            if request.background.is_some() {
                let shell = state.sessions.entry(session.clone()).or_default();
                shell
                    .background
                    .insert(request.task.clone(), context.operation.clone());
            }
        }
        let job = supervise::Job {
            operation: context.operation,
            session,
            plan,
            request,
            running,
            mounted,
            limit: self.limit,
        };
        let shared = self.shared.clone();
        drop(
            self.scope
                .spawner()
                .spawn("shell", supervise::run(shared, launched, job)),
        );
        Ok(())
    }

    pub(crate) fn watch(
        &self,
        operation: &OperationId,
        cursor: u64,
    ) -> Option<BoxStream<'static, Result<CommandEvent, EnvError>>> {
        self.command(operation)
            .map(|running| watch::watch(running, cursor))
    }

    /// 取消是一条消息：发出后由监督任务结束进程树并写终态。已结束或不认识的操作什么都不做。
    pub(crate) fn cancel(&self, operation: &OperationId) -> bool {
        match self.command(operation) {
            Some(running) => {
                running.cancel.cancel();
                true
            }
            None => false,
        }
    }

    pub(crate) fn status(&self, operation: &OperationId) -> Option<Outcome> {
        self.command(operation).map(|running| {
            running
                .finished
                .borrow()
                .as_ref()
                .map_or(Outcome::Running, |finish| finish.outcome.clone())
        })
    }

    pub(crate) fn shell_state(&self, workspace: &WorkspaceRef, session: &SessionId) -> ShellState {
        let outputs = self.task_outputs(session);
        let state = self.shared.state();
        let shell = state.sessions.get(session);
        let cwd = shell.and_then(|shell| shell.cwd.clone()).map_or_else(
            || workspace.root.clone(),
            |cwd| EnvPath::new(cwd.to_string_lossy()),
        );
        // 在跑的与已结束未 ack 的合在一起（结束的瞬间两边可能都有，按任务去重）。
        let background: BTreeSet<ShellTaskId> = shell
            .map(|shell| {
                let finished = shell.notices.iter().map(|notice| notice.task.clone());
                shell.background.keys().cloned().chain(finished).collect()
            })
            .unwrap_or_default();
        ShellState {
            cwd,
            outputs: Some(EnvPath::new(outputs.to_string_lossy())),
            background: background.into_iter().collect(),
        }
    }

    pub(crate) fn notices(
        &self,
        session: &SessionId,
        after: u64,
    ) -> BoxStream<'static, Result<ShellNotice, EnvError>> {
        notices::stream(self.shared.clone(), session.clone(), after)
    }

    pub(crate) fn ack(&self, session: &SessionId, seq: u64) {
        let mut state = self.shared.state();
        let shell = state.sessions.entry(session.clone()).or_default();
        shell.acked = shell.acked.max(seq);
        shell.notices.retain(|notice| notice.seq > seq);
    }

    /// 会话换了工作目录（EnterWorktree / ExitWorktree，docs/specs/rust-cli/tools-more.md §11.4）：
    /// 下一条命令从 `cwd` 开始。
    pub(crate) fn set_cwd(&self, session: &SessionId, cwd: PathBuf) {
        let mut state = self.shared.state();
        state.sessions.entry(session.clone()).or_default().cwd = Some(cwd);
    }

    /// 会话关闭：`forget` 时取消它还在跑的命令（含后台），丢掉 cwd 与未 ack 的通知。
    pub(crate) fn close_session(&self, session: &SessionId, forget: bool) {
        if !forget {
            return;
        }
        let mut state = self.shared.state();
        for running in state.commands.values() {
            if &running.session == session {
                running.cancel.cancel();
            }
        }
        state.sessions.remove(session);
    }
}

#[cfg(test)]
#[path = "shell_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "shell_hook_tests.rs"]
mod hook_tests;

#[cfg(test)]
#[path = "shell_limit_tests.rs"]
mod limit_tests;

#[cfg(test)]
#[path = "shell_choice_tests.rs"]
mod choice_tests;

#[cfg(test)]
#[path = "shell_embedded_tests.rs"]
mod embedded_tests;

#[cfg(test)]
#[path = "shell_timeout_tests.rs"]
mod timeout_tests;
