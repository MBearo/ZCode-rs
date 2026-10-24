//! `zcode executor` 各个命令的执行（docs/specs/rust-cli/remote-exec.md §19.3；解析在 `executor.rs`）。

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use rust_cli_env_local::LocalEnvironment;
use rust_cli_exec_server::ServerConfig;
use rust_cli_exec_wire::admin::ExecutorStatus;
use rust_cli_exec_wire::handshake::PROTOCOL;
use rust_cli_executor_host::admin::{self, Stopped};
use rust_cli_executor_host::file_ledger::Retention;
use rust_cli_executor_host::proxy::{self, DaemonCommand, ProxyConfig};
use rust_cli_executor_host::resident::{Resident, ResidentConfig, StartError};
use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::machine::MachineEnv;
use rust_cli_kernel_api::mcp::NoMcpTransport;
use rust_cli_node_store::SystemClock;
use rust_cli_paths::{ExecutorPaths, StorageSettings};

use crate::executor::ExecutorCommand;
use crate::output::{eprint_line, print_line, write_lossy};

/// `status` 在常驻进程没在跑时的退出码（LSB 的“程序没在运行”）。
const NOT_RUNNING: u8 = 3;
/// 账本多久清理一次（§19.7）。
const LEDGER_SWEEP: Duration = Duration::from_secs(10 * 60);

#[expect(
    clippy::disallowed_methods,
    reason = "入口 crate 是唯一读取进程事实的位置：执行器拉起的常驻进程、loopback 的子进程都是本进程的二进制（README §4）"
)]
pub(crate) fn this_executable() -> Result<std::path::PathBuf> {
    std::env::current_exe().context("finding this executable")
}

/// 跑一个命令，返回退出码。
pub(crate) async fn run(command: ExecutorCommand) -> u8 {
    let result = match command {
        ExecutorCommand::Resident { daemon } => return resident(daemon).await,
        ExecutorCommand::Proxy => proxy().await,
        ExecutorCommand::Standalone {
            add_dirs,
            find_and_grep,
        } => standalone(add_dirs, find_and_grep).await,
        ExecutorCommand::Status { json } => return status(json).await,
        ExecutorCommand::Stop { if_idle } => return stop(if_idle).await,
    };
    match result {
        Ok(()) => 0,
        Err(error) => {
            eprint_line(&format!("zcode executor: {error:#}"));
            1
        }
    }
}

fn executor_paths(startup: &super::Startup) -> ExecutorPaths {
    super::setup::storage_paths(startup, &StorageSettings::default()).executor()
}

/// 按与 TUI / `-p` 相同的规则建本机的执行环境（shell、出口、内嵌的搜索工具）；返回它与 shell 的名字。
async fn local_environment(
    startup: &super::Startup,
    add_dirs: &[String],
    find_and_grep: bool,
) -> Result<(LocalEnvironment, Option<String>)> {
    let shell = crate::shell::config(startup.env.clone(), &startup.temp, find_and_grep).await;
    let shell_name = shell
        .bash
        .as_ref()
        .and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned());
    let egress = super::setup::egress(startup).await?;
    let additional = add_dirs.iter().map(EnvPath::new).collect();
    let env = LocalEnvironment::new(Some(startup.home.clone()))
        .with_shell(shell)
        .with_web(egress)
        .with_additional_directories(additional);
    Ok((env, shell_name))
}

fn server_config(startup: &super::Startup, shell: Option<String>) -> ServerConfig {
    let mut config =
        ServerConfig::new(super::VERSION, std::env::consts::OS, std::env::consts::ARCH);
    config.home = Some(startup.home.to_string_lossy().into_owned());
    config.shell = shell;
    config
}

/// 运行日志（只写文件：stdout 可能是协议的通道）。
fn start_log(dir: &std::path::Path, startup: &super::Startup, mode: &str) -> crate::log::RunLog {
    let clock = Arc::new(SystemClock::new(startup.zone.clone()));
    let log = crate::log::RunLog::start(dir.to_path_buf(), clock);
    tracing::info!(
        event = "process.start",
        version = super::VERSION,
        mode,
        pid = std::process::id(),
        "process started"
    );
    log
}

fn exit_log(result: &Result<()>) {
    match result {
        Ok(()) => tracing::info!(
            event = "process.exit",
            status = "completed",
            "process exiting"
        ),
        Err(error) => tracing::info!(
            event = "process.exit",
            status = "failed",
            error = %format!("{error:#}"),
            "process exiting"
        ),
    }
}

/// 常驻执行器（§19.5）。`daemon`：代理拉起的，在 stdout 写一行就绪结果（`ready` / `busy` / `error <原因>`）。
async fn resident(daemon: bool) -> u8 {
    let report = |line: &str| {
        if daemon {
            write_lossy(std::io::stdout(), line);
        }
    };
    let startup = match super::capture() {
        Ok(startup) => startup,
        Err(error) => {
            report(&format!("{} {error:#}", proxy::ERROR));
            eprint_line(&format!("zcode executor: {error:#}"));
            return 1;
        }
    };
    let paths = executor_paths(&startup);
    let log = start_log(paths.logs().as_path(), &startup, "executor");
    let code = match start_resident(&startup, paths).await {
        Ok(Ok(resident)) => {
            report(proxy::READY);
            if !daemon {
                eprint_line(&format!(
                    "zcode executor: listening on {}",
                    resident.socket().display()
                ));
            }
            // 一直跑到 stop 或退出信号（§12：不按空闲时间退出）。
            let stop = crate::signals::Stop::listen();
            resident.run(stop.token.cancelled()).await;
            exit_log(&Ok(()));
            0
        }
        Ok(Err(StartError::AlreadyRunning)) => {
            report(proxy::BUSY);
            if daemon {
                0
            } else {
                eprint_line("zcode executor: another executor is already running");
                1
            }
        }
        Ok(Err(StartError::Failed(message))) => {
            report(&format!("{} {message}", proxy::ERROR));
            eprint_line(&format!("zcode executor: {message}"));
            exit_log(&Err(anyhow::anyhow!(message)));
            1
        }
        Err(error) => {
            report(&format!("{} {error:#}", proxy::ERROR));
            eprint_line(&format!("zcode executor: {error:#}"));
            exit_log(&Err(error));
            1
        }
    };
    log.flush().await;
    code
}

async fn start_resident(
    startup: &super::Startup,
    paths: ExecutorPaths,
) -> Result<Result<Resident, StartError>> {
    let (env, shell) = local_environment(startup, &[], true).await?;
    let config = ResidentConfig {
        paths,
        version: super::VERSION.to_owned(),
        executable: this_executable()
            .ok()
            .map(|path| path.to_string_lossy().into_owned()),
        clock: Arc::new(SystemClock::new(startup.zone.clone())),
        retention: Retention::default(),
        ledger_sweep: LEDGER_SWEEP,
    };
    Ok(Resident::start(config, Arc::new(env), server_config(startup, shell)).await)
}

/// `--stdio`：代理（§19.5）。
async fn proxy() -> Result<()> {
    let startup = super::capture()?;
    let paths = executor_paths(&startup);
    let log = start_log(paths.logs().as_path(), &startup, "executor-proxy");
    let config = ProxyConfig {
        paths,
        version: super::VERSION.to_owned(),
        protocol: PROTOCOL,
        daemon: DaemonCommand {
            program: this_executable()?,
            args: vec!["executor".to_owned(), "--daemon".to_owned()],
            env: startup.env.to_vec(),
            cwd: Some(startup.cwd.clone()),
        },
    };
    let result = proxy::proxy(tokio::io::stdin(), tokio::io::stdout(), config)
        .await
        .map_err(anyhow::Error::from);
    exit_log(&result);
    log.flush().await;
    result
}

/// `--stdio --standalone`：R1 的行为，只给 loopback（§18.6）。
async fn standalone(add_dirs: Vec<String>, find_and_grep: bool) -> Result<()> {
    let startup = super::capture()?;
    let paths = super::setup::storage_paths(&startup, &StorageSettings::default());
    let log = start_log(paths.logs().as_path(), &startup, "executor");
    let result = async {
        let (env, shell) = local_environment(&startup, &add_dirs, find_and_grep).await?;
        // MCP 的管道在 R5 才经执行器（remote-exec.md §15.9）：这里不提供。
        let machine = MachineEnv::new(Arc::new(env), Arc::new(NoMcpTransport));
        rust_cli_executor_host::serve_stdio(
            tokio::io::stdin(),
            tokio::io::stdout(),
            machine,
            server_config(&startup, shell),
        )
        .await?;
        Ok(())
    }
    .await;
    exit_log(&result);
    log.flush().await;
    result
}

/// `status --json` 的一行（§19.6）：这个二进制自己的版本与协议，加上常驻进程的状态（没在跑时为 null）。部署时的探测
/// 读它（ssh-connect）。
pub(crate) fn status_report(resident: Option<&ExecutorStatus>) -> serde_json::Value {
    serde_json::json!({
        "version": super::VERSION,
        "protocol": PROTOCOL,
        "executable": this_executable().ok().map(|path| path.to_string_lossy().into_owned()),
        "resident": resident,
    })
}

fn status_lines(status: &ExecutorStatus, socket: &std::path::Path) -> Vec<String> {
    let yes = |value: bool| if value { "yes" } else { "no" };
    vec![
        "zcode executor is running".to_owned(),
        format!(
            "  version   {} (protocol {})",
            status.version, status.protocol
        ),
        format!("  pid       {}", status.pid),
        format!("  instance  {}", status.instance),
        format!("  socket    {}", socket.display()),
        format!("  uptime    {}s", status.uptime_ms / 1000),
        format!("  clients   {}", status.clients),
        format!("  calls     {}", status.calls),
        format!("  streams   {}", status.streams),
        format!("  commands  {}", status.commands),
        format!("  notices   {}", status.notices),
        format!("  idle      {}", yes(status.idle)),
    ]
}

async fn status(json: bool) -> u8 {
    let startup = match super::capture() {
        Ok(startup) => startup,
        Err(error) => {
            eprint_line(&format!("zcode executor: {error:#}"));
            return 1;
        }
    };
    let socket = executor_paths(&startup).socket();
    let resident = match admin::status(socket.as_path()).await {
        Ok(resident) => resident,
        Err(error) => {
            eprint_line(&format!("zcode executor: {error}"));
            return 1;
        }
    };
    let running = resident.is_some();
    if json {
        print_line(&status_report(resident.as_ref()).to_string());
    } else if let Some(status) = &resident {
        for line in status_lines(status, socket.as_path()) {
            print_line(&line);
        }
    } else {
        print_line("zcode executor is not running");
    }
    if running { 0 } else { NOT_RUNNING }
}

async fn stop(if_idle: bool) -> u8 {
    let startup = match super::capture() {
        Ok(startup) => startup,
        Err(error) => {
            eprint_line(&format!("zcode executor: {error:#}"));
            return 1;
        }
    };
    let paths = executor_paths(&startup);
    match admin::stop(paths.socket().as_path(), paths.lock().as_path(), if_idle).await {
        Ok(Stopped::NotRunning) => {
            print_line("zcode executor is not running");
            0
        }
        Ok(Stopped::Stopped(status)) => {
            print_line(&format!("zcode executor stopped (pid {})", status.pid));
            0
        }
        Ok(Stopped::Busy(status)) => {
            print_line(&format!(
                "zcode executor is busy (clients {}, calls {}, streams {}, commands {}, notices {}); not stopped",
                status.clients, status.calls, status.streams, status.commands, status.notices
            ));
            1
        }
        Err(error) => {
            eprint_line(&format!("zcode executor: {error}"));
            1
        }
    }
}
