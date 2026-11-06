//! 进程骨架（工作线 F129，审查 E3；docs/specs/rust-cli/cli-args.md §14.2）：这次启动做哪件事（[`Job`]），tokio 运行时
//! 只建一份，带运行日志的运行由 [`Process`] 开头与收尾（`process.start` / `process.exit` 只写一份），错误只在一处写到 stderr。
//!
//! 以前三种入口（`-p` / TUI、app-server、执行器）各自建运行时、开日志、记 `process.exit`，三处的字段形状不同（app-server
//! 不记状态与错误）；`app_server::dispatch` 还替执行器分派。

pub(crate) mod facts;
pub(crate) mod log;
pub(crate) mod log_writer;
pub(crate) mod os_release;
pub(crate) mod output;
pub(crate) mod signals;
pub(crate) mod temp_dir;
pub(crate) mod time_zone;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use rust_cli_base::clock::SystemClock;

use crate::commands::args::{Invocation, Mode, parse, version_line};
use crate::commands::executor::ExecutorCommand;
use crate::process::output::{eprint_line, eprint_raw, help_width, print_line, print_raw};
use crate::{
    VERSION, assemble::network, commands::desktop_app, commands::executor, commands::flags,
    commands::help, commands::hooks_trust, commands::login, commands::update,
    frontends::app_server,
};

/// 常驻执行器停下后等卡住的阻塞线程的上限。
const RESIDENT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// 这次启动要做的事。
pub(crate) enum Job {
    /// `zcode executor …`（remote-exec.md §12、§19.3）：执行器模式，只拿进程事实。
    Executor(ExecutorCommand),
    /// `zcode app-server --stdio`（app-protocol.md §3.7）。
    AppServer(app_server::Parsed),
    /// `zcode login` / `zcode logout`。
    Account(login::Command),
    /// `zcode hooks trust …`（config.md §4.3）。
    Trust(hooks_trust::TrustCommand),
    /// `zcode update`（update.md §9，F48）。
    Update(update::Command),
    /// `zcode app [folder]`（cli-args.md §16）。
    App(desktop_app::Command),
    /// 交互界面。
    Tui(Box<Invocation>),
    /// `-p`（headless.md）。
    Print(Box<Invocation>),
}

/// 运行时怎样收尾。
enum Shutdown {
    /// 等阻塞线程（缺省）。
    Wait,
    /// 不等：代理读 stdin 的阻塞线程要等下一段输入才返回，常驻进程那一头已经结束时马上退出，让 ssh 会话随之结束、客户端
    /// 尽快重连（不然要等到客户端的下一个心跳写进 stdin）。
    Background,
    /// 最多等这么久：常驻进程停下时，卡在阻塞调用里的线程（例如写一个没有读者的 FIFO）不挡退出。
    Timeout(Duration),
}

impl Job {
    /// 按参数选；用法错误、`--version` 与 `--help` 当场写完，返回退出码。分派的顺序：`executor`、`app-server`、
    /// `login` / `logout`、`hooks trust`、`update`、`app`，其余是常规的命令行。
    pub(crate) fn parse(args: &[String]) -> Result<Self, ExitCode> {
        if let Some(parsed) = executor::parse(args) {
            return match parsed {
                Ok(command) => Ok(Self::Executor(command)),
                Err(usage) => Err(usage_error(&usage)),
            };
        }
        if let Some(parsed) = app_server::parse(args) {
            return match parsed {
                Ok(parsed) => Ok(Self::AppServer(parsed)),
                Err(usage) => Err(usage_error(&usage)),
            };
        }
        // 登录与退出（Node：用法错误也是 `Error: …`、退出码 1）。
        if let Some(parsed) = login::parse(args) {
            return match parsed {
                Ok(command) => Ok(Self::Account(command)),
                Err(usage) => Err(usage_error(&format!("Error: {usage}"))),
            };
        }
        if let Some(parsed) = hooks_trust::parse(args) {
            return match parsed {
                Ok(command) => Ok(Self::Trust(command)),
                Err(text) => {
                    eprint_raw(&text);
                    Err(ExitCode::FAILURE)
                }
            };
        }
        // `zcode update` / `upgrade`（update.md §9，F48）：commander 的报错原样写 stderr，退出码 1。
        if let Some(parsed) = update::parse(args) {
            return match parsed {
                Ok(command) => Ok(Self::Update(command)),
                Err(text) => Err(usage_error(&text)),
            };
        }
        // `zcode app [folder]`（cli-args.md §16）：同 `update`，commander 的报错原样写 stderr，退出码 1。
        if let Some(parsed) = desktop_app::parse(args) {
            return match parsed {
                Ok(command) => Ok(Self::App(command)),
                Err(text) => Err(usage_error(&text)),
            };
        }
        // 缺陷修复（F40）：以前是自拟的 `zcode: unsupported arguments: …`、退出码 2；正确的是 commander 的原文
        // （`error: unknown option '…'` 等）或 `Error: …`，退出码 1（cli-args.md §2.2）。
        let invocation = parse(args).map_err(|message| usage_error(&message))?;
        let print = match invocation.mode {
            Mode::Version => {
                print_line(&version_line(VERSION));
                return Err(ExitCode::SUCCESS);
            }
            // 帮助写 stdout、退出码 0（commander `outputHelp`，cli-args.md §3）。
            Mode::Help => {
                print_raw(&help::render(help_width()));
                return Err(ExitCode::SUCCESS);
            }
            Mode::Print => true,
            Mode::Tui => false,
        };
        // 解析时的警告（`--effort` 的值不认识，cli-args.md §1.4）先写 stderr，照常运行。
        for warning in &invocation.warnings {
            eprint_line(warning);
        }
        let invocation = Box::new(invocation);
        Ok(if print {
            Self::Print(invocation)
        } else {
            Self::Tui(invocation)
        })
    }

    fn shutdown(&self) -> Shutdown {
        match self {
            Self::Executor(ExecutorCommand::Proxy) => Shutdown::Background,
            Self::Executor(ExecutorCommand::Resident { .. }) => {
                Shutdown::Timeout(RESIDENT_SHUTDOWN_TIMEOUT)
            }
            Self::Executor(
                ExecutorCommand::Standalone { .. }
                | ExecutorCommand::Status { .. }
                | ExecutorCommand::Stop { .. },
            )
            | Self::AppServer(_)
            | Self::Account(_)
            | Self::Trust(_)
            | Self::Update(_)
            | Self::App(_)
            | Self::Tui(_)
            | Self::Print(_) => Shutdown::Wait,
        }
    }

    /// 跑完这件事，返回退出码；失败已经写到 stderr。
    async fn run(self) -> u8 {
        let result = match self {
            // 执行器自己写失败（`zcode executor: …`），返回退出码。
            Self::Executor(command) => return crate::commands::executor_run::run(command).await,
            Self::AppServer(parsed) => app_server::run(parsed).await,
            Self::Account(command) => crate::assemble::setup::account(command).await,
            Self::Trust(command) => crate::assemble::setup::trust(command).await,
            Self::Update(command) => {
                let env =
                    rust_cli_base::secret::EnvSnapshot::from(crate::assemble::setup::process_env());
                update::command(command, &env, &mut std::io::stdout()).await
            }
            Self::App(command) => {
                let env =
                    rust_cli_base::secret::EnvSnapshot::from(crate::assemble::setup::process_env());
                desktop_app::command(command, &env, &mut std::io::stdout()).await
            }
            Self::Tui(invocation) => crate::frontends::tui::run(*invocation).await,
            Self::Print(invocation) => crate::frontends::print::run(*invocation).await,
        };
        match result {
            Ok(code) => code,
            Err(error) => {
                report(&error);
                1
            }
        }
    }
}

fn usage_error(text: &str) -> ExitCode {
    eprint_line(text);
    ExitCode::FAILURE
}

/// 错误写到 stderr（退出码 1）。
fn report(error: &anyhow::Error) {
    // 标准代理变量格式不对：原样写这几行、不带前缀，退出码 1（config.md §2.4）；参数错误同样原样（`Error: …`，
    // cli-args.md §2.4）。
    if let Some(invalid) = error.downcast_ref::<network::InvalidProxy>() {
        eprint_line(&invalid.to_string());
    } else if let Some(fatal) = error.downcast_ref::<flags::Fatal>() {
        eprint_line(&fatal.to_string());
    // json / stream-json 的最后一个 result 是错误：输出已写，只按退出码 1 结束（headless.md §5）。
    } else if error.is::<rust_cli_headless::ResultIsError>() {
        // stream-json 输入的行错误：固定的文字，退出码 1（headless.md §4）。
    } else if let Some(input) = error.downcast_ref::<rust_cli_headless::InputError>() {
        eprint_line(&input.0);
    } else {
        eprint_line(&format!("zcode: {error:#}"));
    }
}

/// 进程的入口：选事、建运行时、跑完、按这件事的方式收尾。
pub(crate) fn main(args: &[String]) -> ExitCode {
    let job = match Job::parse(args) {
        Ok(job) => job,
        Err(code) => return code,
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprint_line(&format!("zcode: cannot start the async runtime: {error}"));
            return ExitCode::FAILURE;
        }
    };
    let shutdown = job.shutdown();
    let code = runtime.block_on(job.run());
    match shutdown {
        Shutdown::Wait => drop(runtime),
        Shutdown::Background => runtime.shutdown_background(),
        Shutdown::Timeout(limit) => runtime.shutdown_timeout(limit),
    }
    ExitCode::from(code)
}

/// `process.start` 的模式与（app-server 的）展示形态。
pub(crate) struct Start<'a> {
    pub(crate) mode: &'a str,
    pub(crate) surface: Option<&'a str>,
}

/// 一次带运行日志的运行（logging.md §4、§7）：开始时开日志、记 `process.start`；结束时记 `process.exit`（`status`、
/// `code`，失败时 `error`），等日志写完。只写文件：TUI 占着终端，`-p` 的 stdout 是输出，app-server 与执行器的 stdout
/// 是协议。
pub(crate) struct Process {
    log: crate::process::log::RunLog,
}

impl Process {
    pub(crate) fn begin(dir: PathBuf, clock: Arc<SystemClock>, start: Start<'_>) -> Self {
        let log = crate::process::log::RunLog::start(dir, clock);
        let pid = std::process::id();
        match start.surface {
            Some(surface) => tracing::info!(
                event = "process.start",
                version = VERSION,
                mode = start.mode,
                surface,
                pid,
                "process started"
            ),
            None => tracing::info!(
                event = "process.start",
                version = VERSION,
                mode = start.mode,
                pid,
                "process started"
            ),
        }
        Self { log }
    }

    /// 记 `process.exit`（`error` 有值时是 `failed`），等写者写完。
    pub(crate) async fn finish(self, code: u8, error: Option<&anyhow::Error>) {
        match error {
            None => tracing::info!(
                event = "process.exit",
                status = "completed",
                code,
                "process exiting"
            ),
            Some(error) => tracing::info!(
                event = "process.exit",
                status = "failed",
                code,
                error = %format!("{error:#}"),
                "process exiting"
            ),
        }
        self.log.flush().await;
    }
}

/// controller 的一次运行（交互界面、`-p`、app-server）：捕获事实、开日志、听退出信号、建模型轨迹的写者；结束时按退出信号
/// 定退出码（session-lifecycle.md §3.5：0 / 143 / 129），先等模型轨迹写完（在 `process.exit` 之前，model-io.md §6），再
/// 收尾日志。
pub(crate) struct ControllerRun {
    pub(crate) facts: crate::process::facts::Facts,
    /// 读配置之前的存储位置（运行日志、平台层、TUI 的输入历史）。
    pub(crate) paths: rust_cli_paths::StoragePaths,
    pub(crate) clock: Arc<SystemClock>,
    pub(crate) stop: crate::process::signals::Stop,
    pub(crate) model_io: crate::assemble::model_io::ModelIo,
    process: Process,
}

impl ControllerRun {
    pub(crate) fn begin(start: Start<'_>) -> anyhow::Result<Self> {
        let facts = crate::process::facts::Facts::capture()?;
        let paths = facts.storage_paths(&rust_cli_paths::StorageSettings::default());
        let clock = Arc::new(SystemClock::new(facts.process.zone.clone()));
        let process = Process::begin(paths.logs().as_path().to_path_buf(), clock.clone(), start);
        Ok(Self {
            facts,
            paths,
            clock,
            // 退出信号：前端收到令牌后走已有的退出路径。
            stop: crate::process::signals::Stop::listen(),
            model_io: crate::assemble::model_io::ModelIo::default(),
            process,
        })
    }

    /// 收尾：退出码按收到的退出信号（没有时是 `result` 的）。
    pub(crate) async fn finish(self, result: anyhow::Result<u8>) -> anyhow::Result<u8> {
        self.model_io.flush().await;
        let result = result.map(|code| self.stop.code().unwrap_or(code));
        match &result {
            Ok(code) => self.process.finish(*code, None).await,
            Err(error) => self.process.finish(1, Some(error)).await,
        }
        result
    }
}
