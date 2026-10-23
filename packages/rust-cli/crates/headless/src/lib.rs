//! 无界面模式：`-p` 与 stream-json，形状照 print 模式（docs/specs/rust-cli/headless.md）。
//! 也是做请求差分的入口。
//!
//! 输出格式 text / json / stream-json（headless.md §3）；输入格式 text（prompt 参数）与 stream-json（stdin 每行一条
//! user 消息，§4）。

use std::sync::Arc;

use anyhow::{Result, bail};
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::ids::{CommandId, SessionId};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{
    Ack, Command, CommandEnvelope, CreateSession, EndReason, Kernel, KernelError, SessionHandle,
    StartSource,
};
use rust_cli_kernel_api::store::SessionFilter;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite};
use tokio_util::sync::CancellationToken;

mod context;
mod drive;
mod events;
mod format;
mod input;
mod local;
mod partial;
mod printer;
mod usage;

pub use context::markdown as context_markdown;
pub use format::{InputFormat, OutputFormat, PrintFormat};
pub use printer::{Output, Stamps};

use drive::{Asked, Driver};
use printer::Printer;

/// 一次 `-p` 运行的参数。
#[derive(Debug, Clone)]
pub struct PrintOptions {
    pub prompt: String,
    /// `--session-id`（docs/specs/rust-cli/cli-args.md §7）：新会话用这个 ID（入口已换成 `sess_<uuid>` 并查过不重复）。
    pub session_id: Option<SessionId>,
    pub workspace: WorkspaceRef,
    pub selection: ModelSelection,
    pub mode: PermissionMode,
    pub target: SessionTarget,
    /// `--fork-session`：打开后先 fork，在新会话上续写（session-lifecycle.md §1）。
    pub fork: bool,
    /// 入口收到退出信号时取消（session-lifecycle.md §3.5）：中止在跑的回合，等它结束后返回。
    pub stop: CancellationToken,
    /// 输出格式与事件的标记（headless.md §3）。
    pub output: Output,
    /// 退出信号是用户中断（SIGINT、Windows 的 Ctrl+C）时先于 `stop` 取消（修复线 F38）：以用户中断中止
    /// （`Interrupt`，输入与中断标记照常写）；没有取消时按 `Shutdown` 中止（SIGTERM / SIGHUP 不中止 query，
    /// 之后这个回合不再记录）。
    pub cancel: CancellationToken,
}

/// 提问发到哪个会话（docs/specs/rust-cli/session-lifecycle.md §1）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionTarget {
    New,
    /// `-c` / `--continue`：当前工作区最近更新的会话。
    Continue,
    /// `-r` / `--resume <id>`。
    Resume(SessionId),
}

/// json / stream-json 输出时最后一个 result 是错误：输出已经写好，入口只按退出码 1 结束，不另写 stderr
/// （headless.md §5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResultIsError;

impl std::fmt::Display for ResultIsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("the last result is an error")
    }
}

impl std::error::Error for ResultIsError {}

/// stream-json 输入的一行不合规（headless.md §4）：文字是写到 stderr 的原文，入口原样写出、退出码 1。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputError(pub String);

impl std::fmt::Display for InputError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for InputError {}

/// 提问发到的会话：续写的会话（[`existing_session`]），没有时建新会话。
pub(crate) async fn session_for(
    kernel: &dyn Kernel,
    options: &PrintOptions,
) -> Result<Arc<dyn SessionHandle>> {
    if let Some(session) = existing_session(kernel, options).await? {
        return Ok(session);
    }
    let id = kernel
        .create_session(CreateSession {
            id: options.session_id.clone(),
            workspace: options.workspace.clone(),
            selection: options.selection.clone(),
            mode: options.mode,
            plan: false,
        })
        .await?;
    Ok(kernel.open_session(&id).await?)
}

/// 打开要续写的会话：`-r` 找不到时按固定的原文报错；新会话与找不到会话的 `-c` 是 None。
///
/// 修 bug：以前 `-c` 找不到会话时报 `No conversation found to continue`、退出码 1。这句只属于交互界面；
/// print 模式找不到会话时照常建新会话回答（跑分框架第一次运行就可能带 `--continue`）。`--fork-session` 没有可
/// fork 的会话，也直接用新会话（session-lifecycle.md §1）。
pub(crate) async fn existing_session(
    kernel: &dyn Kernel,
    options: &PrintOptions,
) -> Result<Option<Arc<dyn SessionHandle>>> {
    let id = match &options.target {
        SessionTarget::New => return Ok(None),
        SessionTarget::Continue => {
            let filter = SessionFilter {
                workspace: options.workspace.clone(),
                limit: 1,
            };
            match kernel.list_sessions(filter).await?.into_iter().next() {
                Some(latest) => latest.id,
                None => return Ok(None),
            }
        }
        SessionTarget::Resume(id) => id.clone(),
    };
    let session = match kernel.open_session(&id).await {
        Err(KernelError::NotFound { .. }) if matches!(options.target, SessionTarget::Resume(_)) => {
            // `--session-id <uuid>` 建的会话 ID 是 `sess_<uuid>`（cli-args.md §7）：`-r <uuid>` 也能续写它。
            let prefixed = SessionId::new(format!("sess_{id}"));
            match kernel.open_session(&prefixed).await {
                Err(KernelError::NotFound { .. }) => {
                    bail!("No conversation found with session ID: {id}")
                }
                opened => opened?,
            }
        }
        opened => opened?,
    };
    if !options.fork {
        return Ok(Some(session));
    }
    let ack = session
        .send(CommandEnvelope {
            id: CommandId::new(format!("{id}:fork-session")),
            command: Command::Fork { at: None },
        })
        .await;
    let Ack::Accepted { result } = ack else {
        bail!("the session could not be forked: {ack:?}");
    };
    let child = SessionId::new(result.as_str().unwrap_or_default());
    Ok(Some(
        kernel.open_session_as(&child, StartSource::Fork).await?,
    ))
}

/// 跑一次 `-p`：本地命令（`/usage`、`/context`、`/rename`）直接给出结果；其余提交 prompt，跑完这个回合以及
/// 后台命令通知开启的后续回合，按 `options.output` 的格式写到 `output`（headless.md §3）。
pub async fn run(
    kernel: Arc<dyn Kernel>,
    options: PrintOptions,
    output: &mut (impl AsyncWrite + Unpin),
) -> Result<()> {
    let started = std::time::Instant::now();
    let mut printer = Printer::new(options.output.clone(), output);
    // `/usage`：非交互的定义只输出会话部分，不开回合（docs/specs/rust-cli/usage.md §6）。
    if usage::is_usage(&options.prompt) {
        let text = usage::text(kernel.clone(), &options, started).await?;
        let id = local::session_id(&options);
        return local::finish(
            &kernel,
            &options,
            &mut printer,
            ("usage", &id),
            text,
            started,
        )
        .await;
    }
    // `/context`：不开回合，也不为它建会话（context-usage.md §1）。
    if context::is_context(&options.prompt) {
        let text = context::text(kernel.clone(), options.clone()).await?;
        let id = local::session_id(&options);
        return local::finish(
            &kernel,
            &options,
            &mut printer,
            ("context", &id),
            text,
            started,
        )
        .await;
    }
    let session = session_for(kernel.as_ref(), &options).await?;
    let result = match rename_command(&options.prompt) {
        Some(command) => match rename(&session, command).await {
            Ok(text) => {
                let id = session.id().as_str().to_owned();
                local::finish(
                    &kernel,
                    &options,
                    &mut printer,
                    ("rename", &id),
                    text,
                    started,
                )
                .await
            }
            Err(error) => Err(error),
        },
        None => {
            let mut driver = Driver {
                kernel: kernel.clone(),
                session: session.clone(),
                printer: &mut printer,
                stop: options.stop.clone(),
                cancel: options.cancel.clone(),
                input_closed: true,
            };
            match driver.ask(options.prompt.clone()).await {
                Ok(asked) => finish(&mut printer, asked).await,
                Err(error) => Err(error),
            }
        }
    };
    // SessionEnd(other)：`-p` 结束时（session-lifecycle.md §3.3）；失败的 hook 不影响结果。
    let _ = session.close(EndReason::Other).await;
    result
}

/// `--input-format stream-json`（headless.md §4）：stdin 每行一条 user 消息，一行一个回合，按顺序跑；第一条消息
/// 到来时才建（或打开）会话。stdin 结束后等在跑的回合结束，退出码按最后一个 result。
pub async fn run_stream(
    kernel: Arc<dyn Kernel>,
    options: PrintOptions,
    input: impl AsyncBufRead + Unpin,
    output: &mut (impl AsyncWrite + Unpin),
) -> Result<()> {
    let mut printer = Printer::new(options.output.clone(), output);
    let mut lines = input.lines();
    let mut session: Option<Arc<dyn SessionHandle>> = None;
    let mut outcome = Ok(());
    let mut stopped = false;
    loop {
        let line = tokio::select! {
            line = lines.next_line() => line,
            () = options.stop.cancelled() => {
                stopped = true;
                break;
            }
        };
        let line = match line {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(error) => {
                outcome = Err(error.into());
                break;
            }
        };
        let prompt = match input::parse(&line) {
            Ok(input::Line::Skip) => continue,
            Ok(input::Line::Prompt(text)) => text,
            Err(text) => {
                outcome = Err(InputError(text).into());
                break;
            }
        };
        let current = match &session {
            Some(current) => current.clone(),
            None => match session_for(kernel.as_ref(), &options).await {
                Ok(opened) => {
                    session = Some(opened.clone());
                    opened
                }
                Err(error) => {
                    outcome = Err(error);
                    break;
                }
            },
        };
        let mut driver = Driver {
            kernel: kernel.clone(),
            session: current,
            printer: &mut printer,
            stop: options.stop.clone(),
            cancel: options.cancel.clone(),
            input_closed: false,
        };
        match driver.ask(prompt).await {
            Ok(Asked::Done | Asked::Failed(_)) => {}
            Ok(Asked::Stopped) => {
                stopped = true;
                break;
            }
            Err(error) => {
                outcome = Err(error);
                break;
            }
        }
    }
    if let Some(session) = &session {
        let _ = session.close(EndReason::Other).await;
    }
    outcome?;
    printer.finish().await?;
    // 收到退出信号：退出码按信号（session-lifecycle.md §3.5），被中断的回合的 result 不算错误。
    if printer.failed() && !stopped {
        return Err(ResultIsError.into());
    }
    Ok(())
}

/// 一次提问结束：写 text / json 的输出；最后一个 result 是错误时返回错误（headless.md §3.2、§5）。
async fn finish<W: AsyncWrite + Unpin>(printer: &mut Printer<'_, W>, asked: Asked) -> Result<()> {
    match asked {
        Asked::Done => {
            printer.finish().await?;
            Ok(())
        }
        // 收到退出信号：text 不写（session-lifecycle.md §3.5）；json 写被中断的回合的 result。
        Asked::Stopped => {
            if printer.format().output != OutputFormat::Text && printer.has_result() {
                printer.finish().await?;
            }
            Ok(())
        }
        Asked::Failed(message) => {
            printer.finish().await?;
            if printer.format().output == OutputFormat::Text {
                bail!("{message}")
            }
            Err(ResultIsError.into())
        }
    }
}

/// 非交互的 `rename`（docs/specs/rust-cli/titles.md §8.6）：整个 prompt 是 `/rename` 或 `/name`，可带名字。
/// `-p` 只认这一个斜杠命令，其余斜杠输入照旧作为普通输入。
fn rename_command(prompt: &str) -> Option<Command> {
    let prompt = prompt.trim();
    let (name, args) = prompt
        .split_once(char::is_whitespace)
        .unwrap_or((prompt, ""));
    if name != "/rename" && name != "/name" {
        return None;
    }
    let title = args.trim();
    Some(if title.is_empty() {
        Command::GenerateName
    } else {
        Command::Rename {
            title: title.to_owned(),
        }
    })
}

/// 改名或起名，不开回合；返回回执的文本（`Session renamed to: …` 等）。起不出名字时是说明文字，
/// 退出码仍是 0（当作命令的结果）。
async fn rename(session: &Arc<dyn SessionHandle>, command: Command) -> Result<String> {
    // 命令 ID 按事件序号区分：改名会发 `TitleChanged`，同一进程里再改名不会被当成重复命令。
    let asked = session.view().await?.last_seq;
    let label = match &command {
        Command::Rename { title } => Some(title.clone()),
        _ => None,
    };
    let id = CommandId::new(format!("{}:print-rename:{asked}", session.id()));
    Ok(
        match (session.send(CommandEnvelope { id, command }).await, label) {
            (Ack::Accepted { .. }, Some(title)) => format!("Session renamed to: {title}"),
            (Ack::Accepted { result }, None) => {
                result["message"].as_str().unwrap_or_default().to_owned()
            }
            (Ack::Rejected { reason }, None) => reason,
            (ack, _) => bail!("the session could not be renamed: {ack:?}"),
        },
    )
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "stream_tests.rs"]
mod stream_tests;
