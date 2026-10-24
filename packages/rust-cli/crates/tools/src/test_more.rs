//! 本工作线工具（docs/specs/rust-cli/tools-more.md）单测共用的工具上下文。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::env::{EnvPath, OperationContext, Origin, WorkspaceRef};
use rust_cli_kernel_api::ids::{
    AgentId, EnvironmentId, Generation, OperationId, OwnerEpoch, RunId, SessionId, ToolUseId,
};
use rust_cli_kernel_api::local_services::{Fetch, Git, LocalServices};
use rust_cli_kernel_api::machine::{
    Background, Checkpoints, Commands, EnvInfo, Files, MachineEnv, Pdf, Search, Shell,
};
use rust_cli_kernel_api::message::ToolResultContent;
use rust_cli_kernel_api::orchestration::Interactions;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::{
    AllPorts, MemoryStore, UnusedEnvironment, local_services, machine,
};
use rust_cli_kernel_api::tool::{ToolOutput, ToolUseContext};
use tokio_util::sync::CancellationToken;

pub(crate) struct Ctx {
    pub(crate) interactions: Arc<dyn Interactions>,
    pub(crate) machine: MachineEnv,
    pub(crate) local: LocalServices,
    pub(crate) mode: PermissionMode,
    pub(crate) agent: Option<AgentId>,
}

impl Ctx {
    pub(crate) fn new(interactions: Arc<dyn Interactions>) -> Self {
        Self {
            interactions,
            machine: machine(Arc::new(UnusedEnvironment)),
            local: local_services(Arc::new(UnusedEnvironment)),
            mode: PermissionMode::Default,
            agent: None,
        }
    }

    /// 工具调用的机器与本机服务都换成 `env`（一个替身实现全部端口）。
    pub(crate) fn set_env<E: AllPorts + 'static>(&mut self, env: Arc<E>) {
        self.machine = machine(env.clone());
        self.local = local_services(env);
    }

    pub(crate) fn build(self) -> ToolUseContext {
        ToolUseContext {
            session: SessionId::new("sess_1"),
            run: RunId::new("run_1"),
            tool_use: ToolUseId::new("toolu_1"),
            agent: self.agent,
            workspace: WorkspaceRef {
                environment: EnvironmentId::new("local"),
                root: EnvPath::new("/Users/dev/work"),
            },
            mode: self.mode,
            cancel: CancellationToken::new(),
            machine: self.machine,
            local: self.local,
            artifacts: Arc::new(MemoryStore::default()),
            interactions: self.interactions,
            operation: OperationContext {
                environment: EnvironmentId::new("local"),
                generation: Generation::new("g1"),
                owner_epoch: OwnerEpoch::LOCAL,
                operation: OperationId::new("op_1"),
                origin: Origin::Agent {
                    session: SessionId::new("sess_1"),
                    run: RunId::new("run_1"),
                },
            },
        }
    }
}

/// 结果的全部文本（多块用换行连接，`tool_reference` 写成 `<ref:名字>`）。
pub(crate) fn output_text(output: &ToolOutput) -> String {
    output
        .content
        .iter()
        .map(|block| match block {
            ToolResultContent::Text { text } => text.clone(),
            ToolResultContent::Image { .. } => "<image>".to_owned(),
            ToolResultContent::Document { .. } => "<document>".to_owned(),
            ToolResultContent::ToolReference { tool_name } => format!("<ref:{tool_name}>"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 只有 home 与若干文本文件的执行环境：`path_facts`、`read_text` 可用，其余操作 `Unsupported`。
pub(crate) struct TextEnv {
    pub(crate) home: String,
    pub(crate) files: std::sync::Mutex<Vec<(String, String)>>,
    /// `fetch_url` 的脚本：URL → 响应；记录收到的 URL。
    pub(crate) pages: Vec<(String, rust_cli_kernel_api::env_web::FetchedUrl)>,
    pub(crate) fetched: std::sync::Mutex<Vec<String>>,
}

impl TextEnv {
    pub(crate) fn new(files: &[(&str, &str)]) -> Self {
        Self {
            home: "/Users/dev".to_owned(),
            files: std::sync::Mutex::new(
                files
                    .iter()
                    .map(|(path, text)| ((*path).to_owned(), (*text).to_owned()))
                    .collect(),
            ),
            pages: Vec::new(),
            fetched: std::sync::Mutex::default(),
        }
    }

    /// 只回答 `fetch_url` 的执行环境。
    pub(crate) fn web(pages: Vec<(&str, rust_cli_kernel_api::env_web::FetchedUrl)>) -> Self {
        let mut env = Self::new(&[]);
        env.pages = pages
            .into_iter()
            .map(|(url, page)| (url.to_owned(), page))
            .collect();
        env
    }

    pub(crate) fn fetched(&self) -> Vec<String> {
        self.fetched
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

fn unsupported() -> rust_cli_kernel_api::env::EnvError {
    rust_cli_kernel_api::env::EnvError::Unsupported {
        what: "test".to_owned(),
    }
}

#[async_trait]
impl EnvInfo for TextEnv {
    async fn facts(
        &self,
        _workspace: &WorkspaceRef,
    ) -> Result<rust_cli_kernel_api::env::EnvFacts, rust_cli_kernel_api::env::EnvError> {
        Err(unsupported())
    }

    async fn path_facts(
        &self,
        _workspace: &WorkspaceRef,
    ) -> Result<rust_cli_kernel_api::env_files::PathFacts, rust_cli_kernel_api::env::EnvError> {
        Ok(rust_cli_kernel_api::env_files::PathFacts {
            home: Some(EnvPath::new(self.home.clone())),
            style: rust_cli_kernel_api::env_files::PathStyle::Posix,
        })
    }
}

#[async_trait]
impl Files for TextEnv {
    async fn read_file(
        &self,
        _workspace: &WorkspaceRef,
        _request: rust_cli_kernel_api::env::ReadFile,
    ) -> Result<rust_cli_kernel_api::env::FileContent, rust_cli_kernel_api::env::EnvError> {
        Err(unsupported())
    }

    async fn write_file(
        &self,
        _workspace: &WorkspaceRef,
        _context: OperationContext,
        _request: rust_cli_kernel_api::env::WriteFile,
    ) -> Result<rust_cli_kernel_api::env::WriteReceipt, rust_cli_kernel_api::env::EnvError> {
        Err(unsupported())
    }

    async fn read_text(
        &self,
        _workspace: &WorkspaceRef,
        request: rust_cli_kernel_api::env_files::ReadText,
    ) -> Result<rust_cli_kernel_api::env_files::TextRead, rust_cli_kernel_api::env::EnvError> {
        let files = self
            .files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some((_, text)) = files.iter().find(|(path, _)| path == request.path.as_str()) else {
            return Err(rust_cli_kernel_api::env::EnvError::NotFound {
                path: request.path.as_str().to_owned(),
            });
        };
        let lines = u64::try_from(text.lines().count()).unwrap_or(u64::MAX);
        let bytes = u64::try_from(text.len()).unwrap_or(u64::MAX);
        Ok(rust_cli_kernel_api::env_files::TextRead::Text(
            rust_cli_kernel_api::env_files::TextSlice {
                content: text.clone(),
                line_count: lines,
                total_lines: lines,
                total_bytes: bytes,
                read_bytes: bytes,
                mtime_ms: 0,
                version: rust_cli_kernel_api::env::FileVersion::new("v1"),
            },
        ))
    }
}

#[async_trait]
impl Commands for TextEnv {
    async fn start_command(
        &self,
        _workspace: &WorkspaceRef,
        _context: OperationContext,
        _request: rust_cli_kernel_api::env::StartCommand,
    ) -> Result<(), rust_cli_kernel_api::env::EnvError> {
        Err(unsupported())
    }

    fn watch_command(
        &self,
        _operation: OperationId,
        _cursor: u64,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<rust_cli_kernel_api::env::CommandEvent, rust_cli_kernel_api::env::EnvError>,
    > {
        use futures_util::StreamExt;
        futures_util::stream::iter([Err(unsupported())]).boxed()
    }

    async fn cancel(
        &self,
        _operation: OperationId,
    ) -> Result<(), rust_cli_kernel_api::env::EnvError> {
        Err(unsupported())
    }

    async fn operation_status(
        &self,
        _operation: OperationId,
    ) -> Result<rust_cli_kernel_api::env::Outcome, rust_cli_kernel_api::env::EnvError> {
        Err(unsupported())
    }

    async fn close_session(
        &self,
        _session: &SessionId,
        _forget: bool,
    ) -> Result<(), rust_cli_kernel_api::env::EnvError> {
        Ok(())
    }
}

#[async_trait]
impl Fetch for TextEnv {
    async fn fetch_url(
        &self,
        _workspace: &WorkspaceRef,
        request: rust_cli_kernel_api::env_web::FetchUrl,
    ) -> Result<rust_cli_kernel_api::env_web::FetchedUrl, rust_cli_kernel_api::env::EnvError> {
        self.fetched
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(request.url.clone());
        self.pages
            .iter()
            .find(|(url, _)| *url == request.url)
            .map(|(_, page)| page.clone())
            .ok_or_else(unsupported)
    }
}

impl Search for TextEnv {}
impl Shell for TextEnv {}
impl Background for TextEnv {}
impl Checkpoints for TextEnv {}
impl Pdf for TextEnv {}
impl Git for TextEnv {}
