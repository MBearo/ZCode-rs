//! 本工作线工具（docs/specs/rust-cli/tools-more.md）单测共用的工具上下文。

use rust_cli_base::sync::LockUnpoisoned;
use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::controller_services::{ControllerServices, Fetch};
use rust_cli_kernel_api::env::{EnvPath, OperationContext, Origin, WorkspaceRef};
use rust_cli_kernel_api::ids::{AgentId, EnvironmentId, Generation, OperationId, RunId, SessionId};
use rust_cli_kernel_api::machine::{EnvInfo, Files, MachineEnv};
use rust_cli_kernel_api::message::ToolResultContent;
use rust_cli_kernel_api::orchestration::{Interactions, SessionOps};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::{
    AllPorts, UnusedEnvironment, controller_services, machine, unsupported_machine,
};
use rust_cli_kernel_api::tool::{ToolOutput, ToolUseContext};

pub(crate) struct Ctx {
    pub(crate) interactions: Arc<dyn Interactions>,
    /// 会话操作：缺省不持有会话（`testing::NoSession`）。
    pub(crate) session_ops: Arc<dyn SessionOps>,
    pub(crate) machine: MachineEnv,
    pub(crate) controller: ControllerServices,
    pub(crate) mode: PermissionMode,
    pub(crate) agent: Option<AgentId>,
}

impl Ctx {
    pub(crate) fn new(interactions: Arc<dyn Interactions>) -> Self {
        Self {
            interactions,
            session_ops: Arc::new(rust_cli_kernel_api::testing::NoSession),
            machine: machine(Arc::new(UnusedEnvironment)),
            controller: controller_services(Arc::new(UnusedEnvironment)),
            mode: PermissionMode::Default,
            agent: None,
        }
    }

    /// 询问与会话操作是同一个替身（它也实现 `SessionOps`，例如 `ScriptedInteractions` 记下 plan 开关）。
    pub(crate) fn with_session<T: Interactions + SessionOps + 'static>(session: Arc<T>) -> Self {
        Self {
            session_ops: session.clone(),
            ..Self::new(session)
        }
    }

    /// 会话操作是这个替身；询问没有人回答（`NoOneToAsk`）。
    pub(crate) fn with_ops(ops: Arc<dyn SessionOps>) -> Self {
        Self {
            session_ops: ops,
            ..Self::new(Arc::new(rust_cli_kernel_api::testing::NoOneToAsk))
        }
    }

    /// 工具调用的机器与本机服务都换成 `env`（一个替身实现全部端口）。
    pub(crate) fn set_env<E: AllPorts + 'static>(&mut self, env: Arc<E>) {
        self.machine = machine(env.clone());
        self.controller = controller_services(env);
    }

    /// 工具调用的机器换成按端口装好的 `machine`（`unsupported_machine().with_*`，F126）；本机服务不变。
    pub(crate) fn set_machine(&mut self, machine: MachineEnv) {
        self.machine = machine;
    }

    /// 机器的环境事实与文件、本机服务的取网页都换成 `env`；其余端口返回 `Unsupported`。
    pub(crate) fn set_text_env(&mut self, env: Arc<TextEnv>) {
        self.machine = unsupported_machine()
            .with_info(env.clone())
            .with_files(env.clone());
        self.controller = controller_services(env);
    }

    pub(crate) fn build(self) -> ToolUseContext {
        ToolUseContext {
            agent: self.agent,
            workspace: WorkspaceRef {
                environment: EnvironmentId::new("local"),
                root: EnvPath::new("/Users/dev/work"),
            },
            mode: self.mode,
            machine: self.machine,
            controller: self.controller,
            interactions: self.interactions,
            session_ops: self.session_ops,
            operation: OperationContext::new(
                EnvironmentId::new("local"),
                Generation::new("g1"),
                OperationId::new("op_1"),
                Origin::Agent {
                    session: SessionId::new("sess_1"),
                    run: RunId::new("run_1"),
                },
            ),
            ..rust_cli_kernel_api::testing::tool_use_context()
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

/// 只有 home 与若干文本文件的执行环境（环境事实、文件与取网页三个端口）：`path_facts`、`read_text`、`fetch_url`
/// 可用，其余操作 `Unsupported`。
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
        self.fetched.lock_unpoisoned().clone()
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
        let files = self.files.lock_unpoisoned();
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
impl Fetch for TextEnv {
    async fn fetch_url(
        &self,
        _workspace: &WorkspaceRef,
        request: rust_cli_kernel_api::env_web::FetchUrl,
    ) -> Result<rust_cli_kernel_api::env_web::FetchedUrl, rust_cli_kernel_api::env::EnvError> {
        self.fetched.lock_unpoisoned().push(request.url.clone());
        self.pages
            .iter()
            .find(|(url, _)| *url == request.url)
            .map(|(_, page)| page.clone())
            .ok_or_else(unsupported)
    }
}
