//! “序列化往返”执行环境（docs/specs/rust-cli/README.md §6 规则 8）。
//!
//! 包住一个真实的执行环境：每个文件与搜索操作的请求与结果都先做一次 serde 编解码再传递，
//! 证明跨端口只传可序列化的 DTO（上云后这些操作走网络，形状不变）。每个端口的实现只要求被包的环境实现同一个端口
//! （remote-exec.md §15.8.1）。

use std::sync::Arc;

use async_trait::async_trait;
use futures_util::stream::BoxStream;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::env::{
    CommandEvent, EnvError, EnvFacts, FileContent, OperationContext, Outcome, ReadFile,
    StartCommand, WorkspaceRef, WriteFile, WriteReceipt,
};
use crate::env_files::{
    DirEntry, FileStat, FileWritten, GlobFiles, GlobMatches, GrepOutcome, GrepSearch, ListDir,
    PathFacts, PutFile, ReadText, StatFile, TextRead,
};
use crate::env_git::{GitStatus, GitStatusRequest};
use crate::env_rewind::{RewindPlan, RewindPrepare, RewindStatus};
use crate::env_shell::{BackgroundInfo, BackgroundStop, BackgroundTask, ShellNotice, ShellState};
use crate::ids::{OperationId, SessionId};
use crate::local_services::{Fetch, Git};
use crate::machine::{Background, Checkpoints, Commands, EnvInfo, Files, Pdf, Search, Shell};

pub struct RoundTripEnv<E>(pub Arc<E>);

fn wire<T: Serialize + DeserializeOwned>(value: T) -> T {
    let text = serde_json::to_string(&value).unwrap_or_else(|error| panic!("serialize: {error}"));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("deserialize {text}: {error}"))
}

fn back<T: Serialize + DeserializeOwned>(result: Result<T, EnvError>) -> Result<T, EnvError> {
    wire(result.map_err(wire)).map(wire)
}

#[async_trait]
impl<E: EnvInfo> EnvInfo for RoundTripEnv<E> {
    async fn facts(&self, workspace: &WorkspaceRef) -> Result<EnvFacts, EnvError> {
        back(self.0.facts(&wire(workspace.clone())).await)
    }

    async fn path_facts(&self, workspace: &WorkspaceRef) -> Result<PathFacts, EnvError> {
        back(self.0.path_facts(&wire(workspace.clone())).await)
    }
}

#[async_trait]
impl<E: Files> Files for RoundTripEnv<E> {
    async fn read_file(
        &self,
        workspace: &WorkspaceRef,
        request: ReadFile,
    ) -> Result<FileContent, EnvError> {
        back(
            self.0
                .read_file(&wire(workspace.clone()), wire(request))
                .await,
        )
    }

    async fn write_file(
        &self,
        workspace: &WorkspaceRef,
        context: OperationContext,
        request: WriteFile,
    ) -> Result<WriteReceipt, EnvError> {
        let result = self.0.write_file(workspace, wire(context), wire(request));
        back(result.await)
    }

    async fn stat(
        &self,
        workspace: &WorkspaceRef,
        request: StatFile,
    ) -> Result<Option<FileStat>, EnvError> {
        back(self.0.stat(workspace, wire(request)).await)
    }

    async fn list_dir(
        &self,
        workspace: &WorkspaceRef,
        request: ListDir,
    ) -> Result<Vec<DirEntry>, EnvError> {
        back(self.0.list_dir(workspace, wire(request)).await)
    }

    async fn read_text(
        &self,
        workspace: &WorkspaceRef,
        request: ReadText,
    ) -> Result<TextRead, EnvError> {
        back(self.0.read_text(workspace, wire(request)).await)
    }

    async fn put_file(
        &self,
        workspace: &WorkspaceRef,
        context: OperationContext,
        request: PutFile,
    ) -> Result<FileWritten, EnvError> {
        let result = self.0.put_file(workspace, wire(context), wire(request));
        back(result.await)
    }
}

#[async_trait]
impl<E: Search> Search for RoundTripEnv<E> {
    async fn glob(
        &self,
        workspace: &WorkspaceRef,
        request: GlobFiles,
    ) -> Result<GlobMatches, EnvError> {
        back(self.0.glob(workspace, wire(request)).await)
    }

    async fn grep(
        &self,
        workspace: &WorkspaceRef,
        request: GrepSearch,
    ) -> Result<GrepOutcome, EnvError> {
        back(self.0.grep(workspace, wire(request)).await)
    }
}

#[async_trait]
impl<E: Commands> Commands for RoundTripEnv<E> {
    async fn start_command(
        &self,
        workspace: &WorkspaceRef,
        context: OperationContext,
        request: StartCommand,
    ) -> Result<(), EnvError> {
        let result = self
            .0
            .start_command(workspace, wire(context), wire(request));
        back(result.await)
    }

    fn watch_command(
        &self,
        operation: OperationId,
        cursor: u64,
    ) -> BoxStream<'static, Result<CommandEvent, EnvError>> {
        self.0.watch_command(operation, cursor)
    }

    async fn cancel(&self, operation: OperationId) -> Result<(), EnvError> {
        back(self.0.cancel(wire(operation)).await)
    }

    async fn operation_status(&self, operation: OperationId) -> Result<Outcome, EnvError> {
        back(self.0.operation_status(wire(operation)).await)
    }

    async fn close_session(&self, session: &SessionId, forget: bool) -> Result<(), EnvError> {
        back(self.0.close_session(&wire(session.clone()), forget).await)
    }
}

#[async_trait]
impl<E: Shell> Shell for RoundTripEnv<E> {
    async fn shell_state(
        &self,
        workspace: &WorkspaceRef,
        session: &SessionId,
    ) -> Result<ShellState, EnvError> {
        back(self.0.shell_state(workspace, &wire(session.clone())).await)
    }

    fn shell_notices(
        &self,
        session: &SessionId,
        after: u64,
    ) -> BoxStream<'static, Result<ShellNotice, EnvError>> {
        use futures_util::StreamExt;
        self.0
            .shell_notices(&wire(session.clone()), after)
            .map(back)
            .boxed()
    }

    async fn ack_shell_notice(&self, session: &SessionId, seq: u64) -> Result<(), EnvError> {
        back(self.0.ack_shell_notice(&wire(session.clone()), seq).await)
    }

    async fn set_shell_cwd(
        &self,
        session: &SessionId,
        cwd: crate::env::EnvPath,
    ) -> Result<(), EnvError> {
        back(
            self.0
                .set_shell_cwd(&wire(session.clone()), wire(cwd))
                .await,
        )
    }
}

#[async_trait]
impl<E: Background> Background for RoundTripEnv<E> {
    async fn background_command(
        &self,
        request: BackgroundTask,
    ) -> Result<Option<BackgroundInfo>, EnvError> {
        back(self.0.background_command(wire(request)).await)
    }

    async fn stop_background_command(
        &self,
        context: OperationContext,
        request: BackgroundTask,
    ) -> Result<BackgroundStop, EnvError> {
        back(
            self.0
                .stop_background_command(wire(context), wire(request))
                .await,
        )
    }
}

#[async_trait]
impl<E: Checkpoints> Checkpoints for RoundTripEnv<E> {
    async fn rewind_prepare(
        &self,
        workspace: &WorkspaceRef,
        context: OperationContext,
        request: RewindPrepare,
    ) -> Result<RewindPlan, EnvError> {
        let result = self
            .0
            .rewind_prepare(workspace, wire(context), wire(request));
        back(result.await)
    }

    async fn rewind_apply(&self, token: &OperationId) -> Result<RewindStatus, EnvError> {
        back(self.0.rewind_apply(&wire(token.clone())).await)
    }

    async fn rewind_status(&self, token: &OperationId) -> Result<RewindStatus, EnvError> {
        back(self.0.rewind_status(&wire(token.clone())).await)
    }

    async fn rewind_finish(&self, token: &OperationId) -> Result<(), EnvError> {
        back(self.0.rewind_finish(&wire(token.clone())).await)
    }
}

#[async_trait]
impl<E: Pdf> Pdf for RoundTripEnv<E> {
    async fn pdf_page_count(
        &self,
        workspace: &WorkspaceRef,
        request: crate::env_pdf::PdfPageCount,
    ) -> Result<Option<u64>, EnvError> {
        back(self.0.pdf_page_count(workspace, wire(request)).await)
    }

    async fn render_pdf_pages(
        &self,
        workspace: &WorkspaceRef,
        request: crate::env_pdf::RenderPdfPages,
    ) -> Result<crate::env_pdf::PdfRender, EnvError> {
        back(self.0.render_pdf_pages(workspace, wire(request)).await)
    }
}

#[async_trait]
impl<E: Git> Git for RoundTripEnv<E> {
    async fn git_status(
        &self,
        workspace: &WorkspaceRef,
        request: GitStatusRequest,
    ) -> Result<GitStatus, EnvError> {
        back(self.0.git_status(workspace, wire(request)).await)
    }

    async fn git_diff(
        &self,
        workspace: &WorkspaceRef,
        request: crate::env_git::GitDiffRequest,
    ) -> Result<crate::env_git::GitDiff, EnvError> {
        back(self.0.git_diff(workspace, wire(request)).await)
    }

    async fn worktree_create(
        &self,
        context: OperationContext,
        request: crate::env_worktree::WorktreeCreate,
    ) -> Result<crate::env_worktree::WorktreeCreated, EnvError> {
        back(self.0.worktree_create(wire(context), wire(request)).await)
    }

    async fn worktree_list(
        &self,
        cwd: crate::env::EnvPath,
    ) -> Result<crate::env_worktree::WorktreeList, EnvError> {
        back(self.0.worktree_list(wire(cwd)).await)
    }

    async fn worktree_changes(
        &self,
        request: crate::env_worktree::WorktreeInspect,
    ) -> Result<Option<crate::env_worktree::WorktreeChanges>, EnvError> {
        back(self.0.worktree_changes(wire(request)).await)
    }

    async fn worktree_remove(
        &self,
        context: OperationContext,
        request: crate::env_worktree::WorktreeRemove,
    ) -> Result<bool, EnvError> {
        back(self.0.worktree_remove(wire(context), wire(request)).await)
    }
}

#[async_trait]
impl<E: Fetch> Fetch for RoundTripEnv<E> {
    async fn fetch_url(
        &self,
        workspace: &WorkspaceRef,
        request: crate::env_web::FetchUrl,
    ) -> Result<crate::env_web::FetchedUrl, EnvError> {
        back(self.0.fetch_url(workspace, wire(request)).await)
    }
}
