//! 启动与监督一条命令：建输出文件、拼脚本、经 sandbox 后端与进程账本启动，等它结束、超时或被
//! 取消，再写回 cwd 与终态（docs/specs/rust-cli/shell-permissions.md §2.1、§2.2）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::time::Duration;

use rust_cli_kernel_api::env::{EnvError, EnvPath, Outcome, StartCommand};
use rust_cli_kernel_api::env_files::PathStyle;
use rust_cli_kernel_api::env_path::git_bash_spelling;
use rust_cli_kernel_api::env_shell::{BackgroundKind, CwdScope, ShellNotice};
use rust_cli_kernel_api::ids::{OperationId, SessionId};
use rust_cli_proc::ledger::{Containment, Isolation, Ledger, Purpose, Tracked};
use rust_cli_proc::shell::single_quote;
use rust_cli_proc::tree::Shutdown;

use super::sandbox::{self, Backend};
use super::{Finish, OutputLimit, Running, Shared};

/// 超时被结束的退出码（143，SIGTERM）。
const TIMEOUT_EXIT: i32 = 143;
/// 被取消的退出码（137，SIGKILL）。
const CANCEL_EXIT: i32 = 137;

/// 启动前算好的位置与环境。
pub(crate) struct Plan {
    pub dir: PathBuf,
    pub output: PathBuf,
    /// hook 命令的 stderr 单独写这里（`StartCommand::hook`）；其他命令与 stdout 合流。
    pub errors: Option<PathBuf>,
    /// 命令来源的 monitor（工作线 F83）：子进程的 stdout、stderr 接管道，stdout 写进这里与输出文件，stderr 加
    /// `[stderr] ` 写进输出文件（[`tee`]）。
    pub events: Option<PathBuf>,
    pub cwd_file: PathBuf,
    /// 实际启动目录。
    pub cwd: PathBuf,
    /// `StartCommand::cwd`：越界时重置到这里。
    pub origin: PathBuf,
    pub bash: PathBuf,
    pub env: BTreeMap<String, String>,
    /// 内嵌搜索工具的 prelude（embedded-search.md §8）；hook 命令与没有内置时是 None。
    pub prelude: Option<String>,
}

/// 监督任务的输入。
pub(crate) struct Job {
    pub operation: OperationId,
    pub session: SessionId,
    pub plan: Plan,
    pub request: StartCommand,
    pub running: Arc<Running>,
    /// 套了 Linux sandbox（登记在 `State::mounts` 里，结束时注销并清理挂载点）。
    pub mounted: bool,
    /// 输出文件的上限（shell-permissions.md §2.2“输出上限”）。
    pub limit: OutputLimit,
}

/// 关掉扩展 glob 的一段：按 shell 路径里的
/// 名字选；都不含时没有。zsh 缺省打开 `BARE_GLOB_QUAL`，`*(e:'…':)` 这类限定符会执行代码，判决链却把它当普通参数。
fn glob_guard(shell: &Path) -> Option<&'static str> {
    let name = shell.to_string_lossy();
    if name.contains("bash") {
        Some("shopt -u extglob 2>/dev/null || true")
    } else if name.contains("zsh") {
        Some("setopt NO_EXTENDED_GLOB NO_BARE_GLOB_QUAL 2>/dev/null || true")
    } else {
        None
    }
}

/// 实际交给 `<shell> -c` 的脚本（关掉扩展 glob、`eval` 用户命令、stdin 接 /dev/null，
/// 成功后把 `pwd -P` 写进 cwd 文件）。hook 命令（`with_stdin`）的 stdin 是输入 JSON，不接 /dev/null，也不加 glob 一段
///（hook 不经这个包装）。`prelude`（内嵌搜索工具的函数，embedded-search.md §8）写在最前面、单独几行：
/// 函数在关掉扩展 glob 之前就已定义。
pub(crate) fn script(
    command: &str,
    cwd_file: Option<&Path>,
    with_stdin: bool,
    shell: &Path,
    prelude: Option<&str>,
) -> String {
    let body = if with_stdin {
        format!("eval {}", single_quote(command))
    } else {
        let eval = format!("eval {} < /dev/null", single_quote(command));
        match glob_guard(shell) {
            Some(guard) => format!("{guard} && {eval}"),
            None => eval,
        }
    };
    let body = match cwd_file {
        Some(file) => format!(
            "{body} && pwd -P >| {}",
            single_quote(&cwd_file_spelling(file, LOCAL_STYLE))
        ),
        None => body,
    };
    match prelude {
        Some(prelude) => format!("{prelude}\n{body}"),
        None => body,
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "env-local 是进程 IO 的入口：输出文件在执行侧（docs/specs/rust-cli/README.md §4）"
)]
async fn open_output(plan: &Plan, path: &Path) -> std::io::Result<std::fs::File> {
    tokio::fs::create_dir_all(&plan.dir).await?;
    let mut options = tokio::fs::OpenOptions::new();
    options.create(true).append(true);
    // 缺陷修复 R2-1：输出只给本用户读（原来按默认权限建，常见是 0644）。
    #[cfg(unix)]
    options.mode(0o600);
    let file = options.open(path).await?;
    Ok(file.into_std().await)
}

fn io(error: &std::io::Error) -> EnvError {
    EnvError::Io {
        message: error.to_string(),
    }
}

/// 启动进程树；失败时不登记命令。返回的布尔值表示套了 Linux sandbox、已登记它的挂载点（结束时由 [`run`]
/// 注销）。
pub(crate) async fn spawn(
    ledger: &Ledger,
    backend: &Backend,
    shared: &Shared,
    plan: &Plan,
    request: &StartCommand,
) -> Result<(Tracked, bool), EnvError> {
    let output = open_output(plan, &plan.output)
        .await
        .map_err(|error| io(&error))?;
    let errors = match &plan.errors {
        Some(path) => open_output(plan, path).await,
        None => output.try_clone(),
    }
    .map_err(|error| io(&error))?;
    let cwd_file = matches!(request.cwd_scope, CwdScope::Session { .. }).then_some(&*plan.cwd_file);
    let hook = request.hook.is_some();
    let text = script(
        &request.command,
        cwd_file,
        hook,
        &plan.bash,
        plan.prelude.as_deref(),
    );
    let tag = format!("ZCODE_SBX_{}", request.task);
    let launch = sandbox::launch(
        request.sandbox.as_ref(),
        backend,
        &plan.bash,
        &text,
        &plan.cwd,
        &tag,
    )?;
    let mut command = rust_cli_proc::command(&launch.program);
    command
        .args(&launch.args)
        .current_dir(&plan.cwd)
        .env_clear()
        .envs(&plan.env)
        .stdin(if hook { Stdio::piped() } else { Stdio::null() })
        .stdout(if plan.events.is_some() {
            Stdio::piped()
        } else {
            Stdio::from(output)
        })
        .stderr(if plan.events.is_some() {
            Stdio::piped()
        } else {
            Stdio::from(errors)
        });
    let mounted = launch.mounts.is_some();
    if let Some(created) = launch.mounts {
        shared.state().mounts.enter(created);
    }
    let spawned = ledger
        .spawn(
            &mut command,
            if hook { Purpose::Hook } else { Purpose::Bash },
            Isolation::Session,
            Containment::OwnJob,
        )
        .await;
    match spawned {
        Ok(tracked) => Ok((tracked, mounted)),
        Err(error) => {
            if mounted {
                leave_sandbox(shared).await;
            }
            Err(io(&error))
        }
    }
}

/// 一个 Linux sandbox 结束：最后一个结束时清掉 bwrap 留下的挂载点。
async fn leave_sandbox(shared: &Shared) {
    let created = shared.state().mounts.leave();
    super::mounts::clean(created).await;
}

/// 退出码：有退出码用退出码；被 SIGTERM 结束记 144，其他信号记 1。
fn exit_code(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        // SIGTERM 在 Linux 与 macOS 上都是 15。
        if status.signal() == Some(15) {
            return 144;
        }
    }
    1
}

enum Ended {
    Exited(std::io::Result<ExitStatus>),
    TimedOut,
    Cancelled,
    /// 输出文件超过上限。
    OutputLimit,
    /// 被要求转到后台（修复线 L-A，tui-input.md §11）。
    Requested,
}

/// 本机执行环境的路径风格（cwd 文件的写法与读回按它）。
const LOCAL_STYLE: PathStyle = if cfg!(windows) {
    PathStyle::Windows
} else {
    PathStyle::Posix
};

/// 写进脚本的 cwd 文件路径：Windows 上是 Git Bash 写法（`C:\x` → `/c/x`，Node `cwd-capture.ts` 的
/// `windowsPathToGitBashPath`），其他平台原样。
///
/// 缺陷修复（工作线 F57）：以前 Windows 上把 `C:\…` 原样写进 `pwd -P >| '…'`，与 Node 不同。
fn cwd_file_spelling(file: &Path, style: PathStyle) -> String {
    let text = file.to_string_lossy();
    match style {
        PathStyle::Windows => git_bash_spelling(&text),
        PathStyle::Posix => text.into_owned(),
    }
}

/// Git Bash 的 `pwd -P` 换回本机路径（Windows；其他平台原样），照 Node `gitBashPathToWindowsPath`
/// （`apps/zcode-cli/packages/contracts/src/path/git-bash.ts`）：`//server/x` → `\\server\x`；`/cygdrive/c/x` 与 `/c/x`
/// → `C:\x`（盘符后开头多余的 `\` 去掉，只剩盘符时是 `C:\`）；其余把 `/` 换成 `\`。
///
/// 缺陷修复（工作线 F57）：以前只认 `/c/…`，Cygwin 的 `/cygdrive/c/…` 与网络共享的 `//server/…` 原样当路径，
/// 会话的 cwd 回不来（不在工作目录里，被重置）。
fn native_cwd(text: &str, style: PathStyle) -> PathBuf {
    if style == PathStyle::Posix {
        return PathBuf::from(text);
    }
    if text.starts_with("//") {
        return PathBuf::from(text.replace('/', "\\"));
    }
    let drive = |text: &str| {
        let bytes = text.as_bytes();
        (bytes.len() >= 2
            && bytes[0] == b'/'
            && bytes[1].is_ascii_alphabetic()
            && (bytes.len() == 2 || bytes[2] == b'/'))
            .then(|| char::from(bytes[1]).to_ascii_uppercase())
    };
    let windows = |letter: char, rest: &str| {
        let rest = rest.replace('/', "\\");
        let rest = rest.trim_start_matches('\\');
        PathBuf::from(format!("{letter}:\\{rest}"))
    };
    if let Some(after) = text.strip_prefix("/cygdrive")
        && let Some(letter) = drive(after)
    {
        return windows(letter, &after[2..]);
    }
    if let Some(letter) = drive(text) {
        return windows(letter, &text[2..]);
    }
    PathBuf::from(text.replace('/', "\\"))
}

/// 输出文件现在的大小；hook 命令加上 stderr 文件。文件还没有或读不到时按 0（watchdog 跳过这一次）。
#[expect(
    clippy::disallowed_methods,
    reason = "env-local 是进程 IO 的入口：查输出文件的大小（docs/specs/rust-cli/README.md §4）"
)]
async fn output_size(plan: &Plan) -> u64 {
    let mut size = 0;
    for path in std::iter::once(&plan.output).chain(plan.errors.as_ref()) {
        size += tokio::fs::metadata(path).await.map_or(0, |meta| meta.len());
    }
    size
}

/// 缺陷修复（审查 R2-18）：输出由子进程直接写文件，原来没有任何上限，失控的命令（例如卡在追加循环里）能写满磁盘。
/// 按 size watchdog 每隔 `every` 查一次大小，超过上限时返回，由调用方结束命令。
async fn exceeds(plan: &Plan, limit: OutputLimit) {
    loop {
        tokio::time::sleep(limit.every).await;
        if output_size(plan).await > limit.bytes {
            return;
        }
    }
}

/// 前台命令因输出超限被结束后删掉输出文件，不在磁盘上留下超限的文件。
#[expect(
    clippy::disallowed_methods,
    reason = "env-local 是进程 IO 的入口：删除超限的输出文件（docs/specs/rust-cli/README.md §4）"
)]
async fn remove_outputs(plan: &Plan) {
    for path in std::iter::once(&plan.output).chain(plan.errors.as_ref()) {
        if let Err(error) = tokio::fs::remove_file(path).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%error, "could not remove an output file past the limit");
        }
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "env-local 是进程 IO 的入口：读回 cwd 文件与输出大小（docs/specs/rust-cli/README.md §4）"
)]
async fn read_back(plan: &Plan) -> (Option<PathBuf>, u64) {
    let cwd = tokio::fs::read_to_string(&plan.cwd_file)
        .await
        .ok()
        .map(|text| text.trim_end_matches(['\n', '\r']).to_owned())
        .filter(|text| !text.is_empty())
        .map(|text| native_cwd(&text, LOCAL_STYLE));
    let _ = tokio::fs::remove_file(&plan.cwd_file).await;
    let size = tokio::fs::metadata(&plan.output)
        .await
        .map_or(0, |meta| meta.len());
    (cwd, size)
}

/// 后台命令结束时在输出文件末尾写一行终态（`[exited with code N]` 或 `[killed]`），
/// 模型用 Read 看输出时能看到命令已经结束。
#[expect(
    clippy::disallowed_methods,
    reason = "env-local 是进程 IO 的入口：输出文件在执行侧（docs/specs/rust-cli/README.md §4）"
)]
async fn mark_exit(plan: &Plan, outcome: &Outcome, exit: Option<i32>) {
    use tokio::io::AsyncWriteExt;
    let line = match (outcome, exit) {
        (Outcome::Completed, Some(code)) => format!("\n[exited with code {code}]\n"),
        (Outcome::Completed | Outcome::Failed { .. }, None) => {
            "\n[exited with code unknown]\n".to_owned()
        }
        _ => "\n[killed]\n".to_owned(),
    };
    let opened = tokio::fs::OpenOptions::new()
        .append(true)
        .open(&plan.output)
        .await;
    if let Ok(mut file) = opened {
        // 修 bug（F35）：tokio 的 `File::write_all` 只把数据交给后台线程就返回，要 flush 才等到写完。以前没有 flush，
        // 终态与通知偶发先于这一行落盘，读输出的一方（`watch_command`、模型 Read 输出文件）看不到它。
        if file.write_all(line.as_bytes()).await.is_ok() {
            let _ = file.flush().await;
        }
    }
}

/// 等命令结束，写回会话 cwd 与终态；后台命令另记一条通知。
/// hook 命令的输入：写完关闭（写 `<输入 JSON>\n` 后关闭 stdin）。命令不读 stdin、
/// 提前退出时写入失败，忽略（只记日志）。
async fn feed(stdin: Option<tokio::process::ChildStdin>, text: Option<&str>) {
    use tokio::io::AsyncWriteExt;
    if let (Some(mut stdin), Some(text)) = (stdin, text) {
        let _ = stdin.write_all(text.as_bytes()).await;
        let _ = stdin.shutdown().await;
    }
}

/// 等这一段：根进程退出、到了 `timeout`、被取消或输出超过上限，先到的为准。
async fn wait_for(tracked: &mut Tracked, timeout: Option<Duration>, job: &Job) -> Ended {
    let wait = tracked.child().wait();
    let deadline = async {
        match timeout {
            Some(after) => tokio::time::sleep(after).await,
            None => std::future::pending().await,
        }
    };
    // 被要求转后台（修复线 L-A）：已经转过的不再等这个信号。
    let mut moved_by = job.running.moved_by.subscribe();
    let requested = async {
        if *job.running.backgrounded.borrow() {
            return std::future::pending().await;
        }
        let _ = moved_by.wait_for(Option::is_some).await;
    };
    tokio::select! {
        status = wait => Ended::Exited(status),
        () = deadline => Ended::TimedOut,
        () = job.running.cancel.cancelled() => Ended::Cancelled,
        () = exceeds(&job.plan, job.limit) => Ended::OutputLimit,
        () = requested => Ended::Requested,
    }
}

/// 前台命令转到后台（shell-permissions.md §1.2.1）：登记成会话的后台命令（`-p` 据此等它，TaskStop 能停它），
/// 再告诉看输出的一方（`CommandEvent::Backgrounded`）。登记在先：工具结果返回时它已经算后台工作。
fn move_to_background(shared: &Shared, job: &Job) {
    {
        let mut state = shared.state();
        let shell = state.sessions.entry(job.session.clone()).or_default();
        shell
            .background
            .insert(job.request.task.clone(), job.operation.clone());
    }
    job.running.backgrounded.send_replace(true);
}

/// monitor 的管道：stdout 写进事件流的文件与输出文件，stderr 每块前加 `[stderr] ` 写进输出文件（tools-more.md §19.5）。
/// 读到两边都结束为止（根进程退出后进程组被结束，管道随之关闭）。
#[expect(
    clippy::disallowed_methods,
    reason = "env-local 是进程 IO 的入口：输出文件在执行侧（docs/specs/rust-cli/README.md §4）"
)]
async fn tee(
    stdout: Option<tokio::process::ChildStdout>,
    stderr: Option<tokio::process::ChildStderr>,
    plan: &Plan,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let Some(events) = &plan.events else {
        return;
    };
    let open = |path: PathBuf| async move {
        let mut options = tokio::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        options.mode(0o600);
        options.open(path).await
    };
    let (Ok(mut events), Ok(mut merged), Ok(mut errors)) = (
        open(events.clone()).await,
        open(plan.output.clone()).await,
        open(plan.output.clone()).await,
    ) else {
        tracing::warn!("could not open the monitor output files");
        return;
    };
    let out = async {
        let Some(mut stdout) = stdout else { return };
        let mut buffer = vec![0_u8; 64 * 1024];
        while let Ok(read) = stdout.read(&mut buffer).await {
            if read == 0 {
                break;
            }
            let _ = events.write_all(&buffer[..read]).await;
            let _ = events.flush().await;
            let _ = merged.write_all(&buffer[..read]).await;
        }
    };
    let err = async {
        let Some(mut stderr) = stderr else { return };
        let mut buffer = vec![0_u8; 64 * 1024];
        while let Ok(read) = stderr.read(&mut buffer).await {
            if read == 0 {
                break;
            }
            let mut chunk = b"[stderr] ".to_vec();
            chunk.extend_from_slice(&buffer[..read]);
            let _ = errors.write_all(&chunk).await;
        }
    };
    tokio::join!(out, err);
}

pub(crate) async fn run(shared: Arc<Shared>, mut tracked: Tracked, job: Job) {
    // monitor 的管道与进程同时处理：管道读完（进程组结束、管道关闭）之后才写终态，事件流在 `Finished` 之前读得到
    // 全部 stdout。
    let stdout = tracked.child().stdout.take();
    let stderr = tracked.child().stderr.take();
    let (piped, piped_done) = tokio::sync::oneshot::channel();
    let pipes = async {
        tee(stdout, stderr, &job.plan).await;
        let _ = piped.send(());
    };
    let ((), ()) = tokio::join!(supervise(shared, tracked, &job, piped_done), pipes);
}

async fn supervise(
    shared: Arc<Shared>,
    mut tracked: Tracked,
    job: &Job,
    piped: tokio::sync::oneshot::Receiver<()>,
) {
    let mut timeout = job.request.timeout_ms.map(Duration::from_millis);
    let stdin = tracked.child().stdin.take();
    let input = job.request.hook.as_ref().map(|hook| hook.stdin.as_str());
    // 输入与等待同时进行：输入超过管道缓冲、命令又不读时，靠超时或取消结束进程后写入随之失败。
    let (ended, ()) = tokio::join!(wait_for(&mut tracked, timeout, job), feed(stdin, input));
    // 修 bug（工作线 F69）：以前前台命令到了超时一律结束，长编译、长测试被杀，模型只能重跑。`on_timeout` 有值时
    // 不结束，转到后台继续跑，之后的期限是后台命令的期限（从这时算起）；结束时照后台命令处理。
    // 取消已经发出（调用被中断）时不转，照旧结束。
    // 修复线 L-A：用户按转后台的键或来了低延迟投递的输入时同样转（tui-input.md §11），之后的期限同超时转后台。
    let moved = match (&ended, &job.request.on_timeout) {
        (Ended::TimedOut | Ended::Requested, Some(moved)) if !job.running.cancel.is_cancelled() => {
            Some(moved)
        }
        _ => None,
    };
    let ended = match moved {
        Some(moved) => {
            move_to_background(&shared, job);
            timeout = moved.deadline_ms.map(Duration::from_millis);
            wait_for(&mut tracked, timeout, job).await
        }
        None => ended,
    };
    // 结束时按后台命令处理的：本来就是后台命令，或者超时转到了后台。
    let background = job
        .request
        .background
        .as_ref()
        .or(moved.map(|moved| &moved.command));
    let mut output_limit = None;
    let (outcome, exit, timed_out) = match ended {
        Ended::Exited(status) => {
            // 根进程已退出：结束组里残留的进程（不留孤儿，见 spec §2.2）。
            let _ = tracked.terminate(Shutdown::Immediate).await;
            match status {
                Ok(status) => (Outcome::Completed, Some(exit_code(status)), false),
                Err(error) => (
                    Outcome::Failed {
                        code: Some("wait".to_owned()),
                        message: error.to_string(),
                    },
                    None,
                    false,
                ),
            }
        }
        Ended::TimedOut => {
            let _ = tracked.terminate(Shutdown::Graceful).await;
            let message = format!(
                "timed out after {} ms",
                timeout.map_or(0, |t| t.as_millis())
            );
            (
                Outcome::Failed {
                    code: Some("timeout".to_owned()),
                    message,
                },
                Some(TIMEOUT_EXIT),
                true,
            )
        }
        // 被要求转后台却没有转（取消已经发出）：照取消结束。
        Ended::Cancelled | Ended::Requested => {
            let _ = tracked.terminate(Shutdown::Graceful).await;
            (Outcome::Cancelled, Some(CANCEL_EXIT), false)
        }
        // 与取消同样结束整棵树，退出码 137；说明文字随终态交出去（`Finished.output_limit`）。
        Ended::OutputLimit => {
            tracing::warn!(
                operation = %job.operation,
                limit = job.limit.bytes,
                "command killed: output file exceeded the limit"
            );
            let _ = tracked.terminate(Shutdown::Graceful).await;
            output_limit = Some(format!(
                "Command killed: output file exceeded {}",
                job.limit.label
            ));
            (Outcome::Cancelled, Some(CANCEL_EXIT), false)
        }
    };
    // 进程组已经结束：等管道读完（没有管道时立即返回）。
    let _ = piped.await;
    if job.mounted {
        leave_sandbox(&shared).await;
    }
    // monitor 的输出文件是事件流（工作线 F83）：不写终态一行，否则它会成为一条事件。
    if background
        .as_ref()
        .is_some_and(|background| background.kind == BackgroundKind::Bash)
    {
        mark_exit(&job.plan, &outcome, exit).await;
    }
    let (cwd, output_bytes) = read_back(&job.plan).await;
    // monitor 的字节数是 stdout 的（终态 summary 分“有没有输出”）。
    let output_bytes = match &job.plan.events {
        Some(events) => file_size(events).await,
        None => output_bytes,
    };
    if output_limit.is_some() && background.is_none() {
        remove_outputs(&job.plan).await;
    }
    // 转到后台的命令结束时不写回会话的当前目录（后台命令不改它），也不报重置。
    let cwd_reset = if moved.is_some() {
        None
    } else {
        record_cwd(&shared, job, cwd)
    };
    let finish = Finish {
        outcome,
        exit_code: exit,
        timed_out,
        cwd_reset,
        output_bytes,
        output_limit,
    };
    if let Some(background) = background {
        let mut state = shared.state();
        let shell = state.sessions.entry(job.session.clone()).or_default();
        shell.background.remove(&job.request.task);
        // TaskStop 停掉的命令不记通知：停止时任务已标成已通知，结束时不再发 task-notification
        // （docs/specs/rust-cli/shell-permissions.md §2.6）。终态仍照常写进回执。
        if !shell.stopped.remove(&job.request.task) {
            shell.next_seq = shell.next_seq.max(shell.acked) + 1;
            shell.notices.push(ShellNotice {
                seq: shell.next_seq,
                operation: job.operation.clone(),
                task: job.request.task.clone(),
                tool_use: background.tool_use.clone(),
                description: background.description.clone(),
                output: EnvPath::new(job.plan.output.to_string_lossy()),
                outcome: finish.outcome.clone(),
                exit_code: finish.exit_code,
                kind: background.kind,
                output_bytes: finish.output_bytes,
            });
        }
    }
    job.running.finished.send_replace(Some(finish));
    shared.notices.send_modify(|count| *count += 1);
}

#[expect(
    clippy::disallowed_methods,
    reason = "env-local 是进程 IO 的入口：输出文件在执行侧（docs/specs/rust-cli/README.md §4）"
)]
async fn file_size(path: &Path) -> u64 {
    tokio::fs::metadata(path).await.map_or(0, |meta| meta.len())
}

/// `CwdScope::Session`：结束目录在某个根里时保留；越界时回到起点并返回起点。
fn record_cwd(shared: &Shared, job: &Job, cwd: Option<PathBuf>) -> Option<EnvPath> {
    let CwdScope::Session { roots } = &job.request.cwd_scope else {
        return None;
    };
    let cwd = cwd?;
    let origin = &job.plan.origin;
    let allowed = cwd == *origin || roots.iter().any(|root| cwd.starts_with(root.as_str()));
    let mut state = shared.state();
    let shell = state.sessions.entry(job.session.clone()).or_default();
    if allowed {
        shell.cwd = Some(cwd);
        None
    } else {
        shell.cwd = Some(origin.clone());
        Some(EnvPath::new(origin.to_string_lossy()))
    }
}

#[cfg(test)]
#[path = "supervise_tests.rs"]
mod tests;
