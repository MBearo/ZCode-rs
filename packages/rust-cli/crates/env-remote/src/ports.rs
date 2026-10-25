//! 8 个端口与 MCP 的实现：每个方法换成一条请求（remote-exec.md §18.3 的消息表）。

use async_trait::async_trait;
use futures_util::stream::BoxStream;
use rust_cli_exec_wire::BodyBuilder;
use rust_cli_exec_wire::envelope::StreamItem;
use rust_cli_exec_wire::reply::Reply;
use rust_cli_exec_wire::request::Request;
use rust_cli_kernel_api::env::{
    CommandEvent, EnvError, EnvFacts, EnvPath, FileContent, FileVersion, OperationContext, Outcome,
    ReadFile, StartCommand, WorkspaceRef, WriteFile, WriteReceipt,
};
use rust_cli_kernel_api::env_files::{
    DirEntry, FileList, FileStat, FileWritten, GlobFiles, GlobMatches, GrepOutcome, GrepSearch,
    ListDir, ListFiles, PathFacts, PutFile, ReadText, StatFile, StatMany, TextRead,
};
use rust_cli_kernel_api::env_pdf::{PdfPageCount, PdfRender, RenderPdfPages};
use rust_cli_kernel_api::env_rewind::{RewindPlan, RewindPrepare, RewindStatus};
use rust_cli_kernel_api::env_shell::{
    BackgroundInfo, BackgroundStop, BackgroundTask, ShellNotice, ShellState,
};
use rust_cli_kernel_api::ids::{OperationId, SessionId};
use rust_cli_kernel_api::machine::{
    Background, Checkpoints, Commands, EnvInfo, Files, Pdf, Search, Shell,
};

use crate::{RemoteEnvironment, from_wire as from, mismatch, to_wire as to};

/// 取出期望的回复（种类已经在 `call` 里核对过；这里对不上只可能是编码有误）。
macro_rules! expect {
    ($reply:expr, $op:literal, $pattern:pat => $value:expr) => {
        match $reply {
            $pattern => $value,
            other => return Err(mismatch($op, other.op().as_str())),
        }
    };
}

fn empty() -> BodyBuilder {
    BodyBuilder::new()
}

fn workspace(workspace: &WorkspaceRef) -> rust_cli_exec_wire::dto::core::Workspace {
    to::workspace(workspace)
}

#[async_trait]
impl EnvInfo for RemoteEnvironment {
    async fn facts(&self, ws: &WorkspaceRef) -> Result<EnvFacts, EnvError> {
        let request = Request::Facts {
            workspace: workspace(ws),
        };
        let (reply, _) = self.call(request, empty()).await?;
        Ok(from::facts(
            expect!(reply, "info.facts", Reply::Facts(facts) => facts),
        ))
    }

    async fn path_facts(&self, ws: &WorkspaceRef) -> Result<PathFacts, EnvError> {
        let request = Request::PathFacts {
            workspace: workspace(ws),
        };
        let (reply, _) = self.call(request, empty()).await?;
        Ok(from::path_facts(
            expect!(reply, "info.path_facts", Reply::PathFacts(facts) => facts),
        ))
    }
}

#[async_trait]
impl Files for RemoteEnvironment {
    async fn read_file(
        &self,
        ws: &WorkspaceRef,
        request: ReadFile,
    ) -> Result<FileContent, EnvError> {
        let request = Request::ReadFile {
            workspace: workspace(ws),
            path: request.path.as_str().to_owned(),
        };
        let (reply, body) = self.call(request, empty()).await?;
        from::file_content(
            expect!(reply, "files.read_file", Reply::ReadFile(content) => content),
            &body,
        )
    }

    async fn write_file(
        &self,
        ws: &WorkspaceRef,
        context: OperationContext,
        request: WriteFile,
    ) -> Result<WriteReceipt, EnvError> {
        let mut body = empty();
        let request = Request::WriteFile {
            workspace: workspace(ws),
            context: to::context(context),
            path: request.path.as_str().to_owned(),
            bytes: body.push(&request.bytes),
            precondition: to::precondition(request.precondition),
        };
        let (reply, _) = self.call(request, body).await?;
        let version = expect!(reply, "files.write_file", Reply::WriteFile { version } => version);
        Ok(WriteReceipt {
            version: FileVersion::new(version),
        })
    }

    async fn stat(
        &self,
        ws: &WorkspaceRef,
        request: StatFile,
    ) -> Result<Option<FileStat>, EnvError> {
        let request = Request::Stat {
            workspace: workspace(ws),
            path: request.path.as_str().to_owned(),
        };
        let (reply, _) = self.call(request, empty()).await?;
        Ok(expect!(reply, "files.stat", Reply::Stat(stat) => stat).map(from::file_stat))
    }

    /// 一次请求（R3，remote-exec.md §20.7.4）；老执行器不认这条消息（`unsupported`）时改成逐个 `files.stat`。
    async fn stat_many(
        &self,
        ws: &WorkspaceRef,
        request: StatMany,
    ) -> Result<Vec<Result<Option<FileStat>, EnvError>>, EnvError> {
        let wire = Request::StatMany {
            workspace: workspace(ws),
            paths: request
                .paths
                .iter()
                .map(|path| path.as_str().to_owned())
                .collect(),
        };
        match self.call(wire, empty()).await {
            Ok((reply, _)) => Ok(
                expect!(reply, "files.stat_many", Reply::StatMany(found) => found)
                    .into_iter()
                    .map(from::stat_entry)
                    .collect(),
            ),
            Err(EnvError::Unsupported { .. }) => {
                let mut out = Vec::with_capacity(request.paths.len());
                for path in request.paths {
                    out.push(self.stat(ws, StatFile { path }).await);
                }
                Ok(out)
            }
            Err(other) => Err(other),
        }
    }

    async fn list_dir(
        &self,
        ws: &WorkspaceRef,
        request: ListDir,
    ) -> Result<Vec<DirEntry>, EnvError> {
        let request = Request::ListDir {
            workspace: workspace(ws),
            path: request.path.as_str().to_owned(),
        };
        let (reply, _) = self.call(request, empty()).await?;
        let entries = expect!(reply, "files.list_dir", Reply::ListDir(entries) => entries);
        Ok(entries.into_iter().map(from::dir_entry).collect())
    }

    async fn read_text(&self, ws: &WorkspaceRef, request: ReadText) -> Result<TextRead, EnvError> {
        let request = Request::ReadText {
            workspace: workspace(ws),
            request: to::read_text(request),
        };
        let (reply, body) = self.call(request, empty()).await?;
        from::text_read(
            expect!(reply, "files.read_text", Reply::ReadText(read) => read),
            &body,
        )
    }

    async fn put_file(
        &self,
        ws: &WorkspaceRef,
        context: OperationContext,
        request: PutFile,
    ) -> Result<FileWritten, EnvError> {
        let mut body = empty();
        let request = Request::PutFile {
            workspace: workspace(ws),
            context: to::context(context),
            request: to::put_file(request, &mut body),
        };
        let (reply, _) = self.call(request, body).await?;
        Ok(from::file_written(
            expect!(reply, "files.put_file", Reply::PutFile(written) => written),
        ))
    }
}

#[async_trait]
impl Search for RemoteEnvironment {
    async fn glob(&self, ws: &WorkspaceRef, request: GlobFiles) -> Result<GlobMatches, EnvError> {
        let request = Request::Glob {
            workspace: workspace(ws),
            request: to::glob(request),
        };
        let (reply, _) = self.call(request, empty()).await?;
        Ok(from::glob_matches(
            expect!(reply, "search.glob", Reply::Glob(matches) => matches),
        ))
    }

    async fn list_files(
        &self,
        ws: &WorkspaceRef,
        request: ListFiles,
    ) -> Result<FileList, EnvError> {
        let request = Request::ListFiles {
            workspace: workspace(ws),
            cwd: request.cwd.as_str().to_owned(),
        };
        let (reply, _) = self.call(request, empty()).await?;
        let files = expect!(reply, "search.list_files", Reply::ListFiles { files } => files);
        Ok(FileList { files })
    }

    async fn grep(&self, ws: &WorkspaceRef, request: GrepSearch) -> Result<GrepOutcome, EnvError> {
        let request = Request::Grep {
            workspace: workspace(ws),
            request: to::grep(request),
        };
        let (reply, _) = self.call(request, empty()).await?;
        Ok(from::grep_outcome(
            expect!(reply, "search.grep", Reply::Grep(outcome) => outcome),
        ))
    }
}

#[async_trait]
impl Commands for RemoteEnvironment {
    async fn start_command(
        &self,
        ws: &WorkspaceRef,
        context: OperationContext,
        request: StartCommand,
    ) -> Result<(), EnvError> {
        let mut body = empty();
        let request = Request::StartCommand {
            workspace: workspace(ws),
            context: to::context(context),
            request: Box::new(to::start_command(request, &mut body)),
        };
        let (reply, _) = self.call(request, body).await?;
        expect!(reply, "commands.start_command", Reply::StartCommand => Ok(()))
    }

    fn watch_command(
        &self,
        operation: OperationId,
        cursor: u64,
    ) -> BoxStream<'static, Result<CommandEvent, EnvError>> {
        let request = Request::WatchCommand {
            operation: operation.as_str().to_owned(),
            cursor,
        };
        self.stream(request, |item, body| match item {
            StreamItem::Command { event } => from::command_event(event, body),
            other => Err(unexpected("commands.watch_command", &other)),
        })
    }

    async fn cancel(&self, operation: OperationId) -> Result<(), EnvError> {
        let request = Request::Cancel {
            operation: operation.as_str().to_owned(),
        };
        let (reply, _) = self.call(request, empty()).await?;
        expect!(reply, "commands.cancel", Reply::Cancel => Ok(()))
    }

    async fn operation_status(&self, operation: OperationId) -> Result<Outcome, EnvError> {
        let request = Request::OperationStatus {
            operation: operation.as_str().to_owned(),
        };
        let (reply, _) = self.call(request, empty()).await?;
        Ok(from::outcome(
            expect!(reply, "commands.operation_status", Reply::OperationStatus(outcome) => outcome),
        ))
    }

    async fn close_session(&self, session: &SessionId, forget: bool) -> Result<(), EnvError> {
        let request = Request::CloseSession {
            session: session.as_str().to_owned(),
            forget,
        };
        let (reply, _) = self.call(request, empty()).await?;
        expect!(reply, "commands.close_session", Reply::CloseSession => Ok(()))
    }
}

fn unexpected(op: &str, item: &StreamItem) -> EnvError {
    let kind = serde_json::to_value(item)
        .ok()
        .and_then(|value| {
            value
                .get("kind")
                .and_then(|kind| kind.as_str().map(str::to_owned))
        })
        .unwrap_or_default();
    EnvError::Io {
        message: format!("executor protocol error: {op} streamed a {kind} item"),
    }
}

#[async_trait]
impl Shell for RemoteEnvironment {
    async fn shell_state(
        &self,
        ws: &WorkspaceRef,
        session: &SessionId,
    ) -> Result<ShellState, EnvError> {
        let request = Request::ShellState {
            workspace: workspace(ws),
            session: session.as_str().to_owned(),
        };
        let (reply, _) = self.call(request, empty()).await?;
        Ok(from::shell_state(
            expect!(reply, "shell.shell_state", Reply::ShellState(state) => state),
        ))
    }

    fn shell_notices(
        &self,
        session: &SessionId,
        after: u64,
    ) -> BoxStream<'static, Result<ShellNotice, EnvError>> {
        let request = Request::ShellNotices {
            session: session.as_str().to_owned(),
            after,
        };
        self.stream(request, |item, _| match item {
            StreamItem::Notice { notice } => Ok(from::shell_notice(notice)),
            other => Err(unexpected("shell.shell_notices", &other)),
        })
    }

    async fn ack_shell_notice(&self, session: &SessionId, seq: u64) -> Result<(), EnvError> {
        let request = Request::AckShellNotice {
            session: session.as_str().to_owned(),
            seq,
        };
        let (reply, _) = self.call(request, empty()).await?;
        expect!(reply, "shell.ack_shell_notice", Reply::AckShellNotice => Ok(()))
    }

    async fn set_shell_cwd(&self, session: &SessionId, cwd: EnvPath) -> Result<(), EnvError> {
        let request = Request::SetShellCwd {
            session: session.as_str().to_owned(),
            cwd: cwd.as_str().to_owned(),
        };
        let (reply, _) = self.call(request, empty()).await?;
        expect!(reply, "shell.set_shell_cwd", Reply::SetShellCwd => Ok(()))
    }
}

#[async_trait]
impl Background for RemoteEnvironment {
    async fn background_command(
        &self,
        task: BackgroundTask,
    ) -> Result<Option<BackgroundInfo>, EnvError> {
        let request = Request::BackgroundCommand {
            task: to::background_task(task),
        };
        let (reply, _) = self.call(request, empty()).await?;
        Ok(
            expect!(reply, "background.background_command", Reply::BackgroundCommand(info) => info)
                .map(from::background_info),
        )
    }

    async fn background_elapsed(&self, task: BackgroundTask) -> Result<Option<u64>, EnvError> {
        let request = Request::BackgroundElapsed {
            task: to::background_task(task),
        };
        let (reply, _) = self.call(request, empty()).await?;
        Ok(
            expect!(reply, "background.background_elapsed", Reply::BackgroundElapsed(elapsed) => elapsed),
        )
    }

    async fn stop_background_command(
        &self,
        context: OperationContext,
        task: BackgroundTask,
    ) -> Result<BackgroundStop, EnvError> {
        let request = Request::StopBackgroundCommand {
            context: to::context(context),
            task: to::background_task(task),
        };
        let (reply, _) = self.call(request, empty()).await?;
        Ok(from::background_stop(
            expect!(reply, "background.stop_background_command", Reply::StopBackgroundCommand(stop) => stop),
        ))
    }
}

#[async_trait]
impl Checkpoints for RemoteEnvironment {
    async fn rewind_prepare(
        &self,
        ws: &WorkspaceRef,
        context: OperationContext,
        request: RewindPrepare,
    ) -> Result<RewindPlan, EnvError> {
        let mut body = empty();
        let request = Request::RewindPrepare {
            workspace: workspace(ws),
            context: to::context(context),
            files: to::rewind_files(request, &mut body),
        };
        let (reply, _) = self.call(request, body).await?;
        Ok(from::rewind_plan(
            expect!(reply, "checkpoints.rewind_prepare", Reply::RewindPrepare(plan) => plan),
        ))
    }

    async fn rewind_apply(&self, token: &OperationId) -> Result<RewindStatus, EnvError> {
        let request = Request::RewindApply {
            token: token.as_str().to_owned(),
        };
        let (reply, _) = self.call(request, empty()).await?;
        Ok(from::rewind_status(
            expect!(reply, "checkpoints.rewind_apply", Reply::RewindApply(status) => status),
        ))
    }

    async fn rewind_status(&self, token: &OperationId) -> Result<RewindStatus, EnvError> {
        let request = Request::RewindStatus {
            token: token.as_str().to_owned(),
        };
        let (reply, _) = self.call(request, empty()).await?;
        Ok(from::rewind_status(
            expect!(reply, "checkpoints.rewind_status", Reply::RewindStatus(status) => status),
        ))
    }

    async fn rewind_finish(&self, token: &OperationId) -> Result<(), EnvError> {
        let request = Request::RewindFinish {
            token: token.as_str().to_owned(),
        };
        let (reply, _) = self.call(request, empty()).await?;
        expect!(reply, "checkpoints.rewind_finish", Reply::RewindFinish => Ok(()))
    }
}

#[async_trait]
impl Pdf for RemoteEnvironment {
    async fn pdf_page_count(
        &self,
        ws: &WorkspaceRef,
        request: PdfPageCount,
    ) -> Result<Option<u64>, EnvError> {
        let request = Request::PdfPageCount {
            workspace: workspace(ws),
            path: request.path.as_str().to_owned(),
        };
        let (reply, _) = self.call(request, empty()).await?;
        Ok(expect!(reply, "pdf.pdf_page_count", Reply::PdfPageCount(count) => count))
    }

    async fn render_pdf_pages(
        &self,
        ws: &WorkspaceRef,
        request: RenderPdfPages,
    ) -> Result<PdfRender, EnvError> {
        let request = Request::RenderPdfPages {
            workspace: workspace(ws),
            request: to::render_pdf_pages(request),
        };
        let (reply, body) = self.call(request, empty()).await?;
        from::pdf_render(
            expect!(reply, "pdf.render_pdf_pages", Reply::RenderPdfPages(render) => render),
            &body,
        )
    }
}
