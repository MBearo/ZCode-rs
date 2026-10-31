//! `zcode executor`（docs/specs/rust-cli/remote-exec.md §12、§18.6、§19.3）：执行器模式。同一个二进制，不读模型凭据、
//! 不打开会话库、不画界面、不写模型轨迹；按与 TUI / `-p` 相同的规则建本机的执行环境（env-local），交给 executor-host。
//!
//! | 命令                                   | 做什么                                                                 |
//! | -------------------------------------- | ---------------------------------------------------------------------- |
//! | `zcode executor`                       | 前台跑常驻执行器（单实例、本用户私有的 socket），一直跑到 `stop`        |
//! | `zcode executor --daemon`              | 同上，另在 stdout 写一行就绪结果（代理拉起它时用，不写进帮助）          |
//! | `zcode executor --stdio`               | 代理：没有常驻进程就拉起一个，再把 stdio 接到它的 socket                |
//! | `zcode executor --stdio --standalone`  | R1 的行为：在自己的 stdio 上直接服务一条连接（只给 loopback，§18.6）    |
//! | `zcode executor status [--json]`       | 常驻进程的状态                                                         |
//! | `zcode executor stop [--if-idle]`      | 停掉常驻进程                                                           |
//!
//! `--add-dir` 与 `--no-find-and-grep` 是 loopback 为了与本机结果相同而传的会话参数，只能和 `--standalone` 一起用。

use std::process::ExitCode;

use crate::output::eprint_line;

/// 子命令的名字。
const NAME: &str = "executor";
/// 常驻进程停下后等卡住的阻塞线程的上限。
const RESIDENT_SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExecutorCommand {
    Resident {
        daemon: bool,
    },
    Proxy,
    Standalone {
        add_dirs: Vec<String>,
        find_and_grep: bool,
    },
    Status {
        json: bool,
    },
    Stop {
        if_idle: bool,
    },
}

fn unknown_option(arg: &str) -> String {
    format!("error: unknown option '{arg}'")
}

/// 第一个参数是 `executor` 时解析其余参数；不是时 None。
pub(crate) fn parse(args: &[String]) -> Option<Result<ExecutorCommand, String>> {
    let (first, rest) = args.split_first()?;
    if first != NAME {
        return None;
    }
    Some(parse_rest(rest))
}

fn parse_rest(rest: &[String]) -> Result<ExecutorCommand, String> {
    match rest.first().map(String::as_str) {
        Some("status") => {
            let mut json = false;
            for arg in &rest[1..] {
                match arg.as_str() {
                    "--json" => json = true,
                    other => return Err(unknown_option(other)),
                }
            }
            return Ok(ExecutorCommand::Status { json });
        }
        Some("stop") => {
            let mut if_idle = false;
            for arg in &rest[1..] {
                match arg.as_str() {
                    "--if-idle" => if_idle = true,
                    other => return Err(unknown_option(other)),
                }
            }
            return Ok(ExecutorCommand::Stop { if_idle });
        }
        Some(other) if !other.starts_with('-') => {
            return Err(format!("error: unknown command '{other}'"));
        }
        _ => {}
    }
    let (mut stdio, mut standalone, mut daemon) = (false, false, false);
    let mut add_dirs = Vec::new();
    let mut find_and_grep = true;
    let mut session_options = false;
    let mut rest = rest.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--stdio" => stdio = true,
            "--standalone" => standalone = true,
            "--daemon" => daemon = true,
            "--no-find-and-grep" => {
                find_and_grep = false;
                session_options = true;
            }
            "--add-dir" => match rest.next() {
                Some(dir) => {
                    add_dirs.push(dir.clone());
                    session_options = true;
                }
                None => {
                    return Err("error: option '--add-dir <directory>' argument missing".to_owned());
                }
            },
            other => return Err(unknown_option(other)),
        }
    }
    if session_options && !standalone {
        return Err(
            "error: --add-dir and --no-find-and-grep only work with --stdio --standalone"
                .to_owned(),
        );
    }
    match (stdio, standalone, daemon) {
        (false, false, daemon) => Ok(ExecutorCommand::Resident { daemon }),
        (true, false, false) => Ok(ExecutorCommand::Proxy),
        (true, true, false) => Ok(ExecutorCommand::Standalone {
            add_dirs,
            find_and_grep,
        }),
        (false, true, _) => Err("error: --standalone needs --stdio".to_owned()),
        (true, _, true) => Err("error: --daemon cannot be used with --stdio".to_owned()),
    }
}

/// 拉起执行器的参数（loopback 用；与 [`parse`] 互为逆）：每个会话一个执行器，总是 `--standalone`（§18.6、§19.3）。
pub(crate) fn child_args(add_dirs: &[String], find_and_grep: bool) -> Vec<String> {
    let mut args = vec![
        NAME.to_owned(),
        "--stdio".to_owned(),
        "--standalone".to_owned(),
    ];
    for dir in add_dirs {
        args.push("--add-dir".to_owned());
        args.push(dir.clone());
    }
    if !find_and_grep {
        args.push("--no-find-and-grep".to_owned());
    }
    args
}

/// 执行器进程的入口：用法错误写 stderr、退出码 1。
pub(crate) fn main(parsed: Result<ExecutorCommand, String>) -> ExitCode {
    let command = match parsed {
        Ok(command) => command,
        Err(usage) => {
            eprint_line(&usage);
            return ExitCode::FAILURE;
        }
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
    let mode = command.clone();
    let code = runtime.block_on(crate::executor_run::run(command));
    match mode {
        // 代理读 stdin 的阻塞线程要等下一段输入才返回：常驻进程那一头已经结束时不等它，马上退出，让 ssh 会话随之结束、
        // 客户端尽快重连（不然要等到客户端的下一个心跳写进 stdin）。
        ExecutorCommand::Proxy => runtime.shutdown_background(),
        // 常驻进程停下时，卡在阻塞调用里的线程（例如写一个没有读者的 FIFO）不挡退出。
        ExecutorCommand::Resident { .. } => runtime.shutdown_timeout(RESIDENT_SHUTDOWN_TIMEOUT),
        ExecutorCommand::Standalone { .. }
        | ExecutorCommand::Status { .. }
        | ExecutorCommand::Stop { .. } => drop(runtime),
    }
    ExitCode::from(code)
}

#[cfg(test)]
#[path = "executor_tests.rs"]
mod tests;
