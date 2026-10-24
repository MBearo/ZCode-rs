//! 子代理的上下文：attachment 由主线程的 [`ContextBuilder`] 按 `QuerySource::Subagent` 产生
//! （context 的子代理分支），system 段换成子代理自己的（docs/specs/rust-cli/agents.md §4.3）。
//! 以及 agent 清单（`ListingSource`，交给主线程的 `agent_listing_delta`）。

use std::sync::{Arc, PoisonError, RwLock};

use async_trait::async_trait;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::listing::{ListingEntry, ListingSource, Listings};
use rust_cli_kernel_api::machine::{EnvInfo, Files};
use rust_cli_kernel_api::message::{AttachmentMessage, Message};
use rust_cli_kernel_api::model::SystemBlock;
use rust_cli_kernel_api::orchestration::{
    BuiltContext, ContextBuilder, ContextRequest, OrchestrationError,
};

use crate::custom;
use crate::definitions::{AgentDefinition, builtins, merge};
use crate::plugin::{self, PluginAgentFile};

pub(crate) struct SubagentContext {
    pub(crate) inner: Arc<dyn ContextBuilder>,
    pub(crate) system: Vec<SystemBlock>,
}

#[async_trait]
impl ContextBuilder for SubagentContext {
    async fn build(
        &self,
        request: ContextRequest,
        history: &[Message],
    ) -> Result<BuiltContext, OrchestrationError> {
        let mut built = self.inner.build(request, history).await?;
        built.system = self.system.clone();
        Ok(built)
    }

    /// 当步的 attachment 交给主线程的实现（子代理来源由它决定给什么，docs/specs/rust-cli/query.md §1.1）。
    async fn turn_attachments(
        &self,
        request: ContextRequest,
        history: &[Message],
    ) -> Result<Vec<AttachmentMessage>, OrchestrationError> {
        self.inner.turn_attachments(request, history).await
    }

    /// 运行开始的一组同样交给主线程的实现（子代理开头的那几项，agents.md §4.3，修复线 F16）。
    /// 不转发时会落到缺省实现（`turn_attachments`），子代理第一个请求变成工具轮那一组的顺序。
    async fn start_attachments(
        &self,
        request: ContextRequest,
        history: &[Message],
    ) -> Result<Vec<AttachmentMessage>, OrchestrationError> {
        self.inner.start_attachments(request, history).await
    }
}

/// 当前可用的 agent 类型：内置、插件、用户级、项目级（同名时后者覆盖）。agent 文件每次都从执行环境
/// 读，文件是唯一事实源，不缓存；插件 agent 的文件位置是插件加载时交进来的快照。
///
/// 主线程的 agent 清单（context）与 Agent 工具共用同一个实例（runtime 组装），两处看到的插件 agent
/// 总是同一组。
pub struct AgentCatalog {
    /// agent 定义是本机的文件（用户级与项目级目录、插件），经本机的端口读。
    info: Arc<dyn EnvInfo>,
    files: Arc<dyn Files>,
    /// 插件 agent 文件；只由 runtime 在插件加载后设置（agents.md §3）。
    plugins: RwLock<Vec<PluginAgentFile>>,
}

impl AgentCatalog {
    pub fn new(info: Arc<dyn EnvInfo>, files: Arc<dyn Files>) -> Self {
        Self {
            info,
            files,
            plugins: RwLock::default(),
        }
    }

    /// 换上插件加载得到的 agent 文件（`LocalKernel::start_extensions`）。
    pub fn set_plugins(&self, files: Vec<PluginAgentFile>) {
        *self.plugins.write().unwrap_or_else(PoisonError::into_inner) = files;
    }

    /// 当前可用的 agent 类型（`/context` 的自定义 agent 也用它，context-usage.md §3）。
    pub async fn definitions(&self, workspace: &WorkspaceRef) -> Vec<AgentDefinition> {
        let files = self
            .plugins
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let mut layers = vec![
            builtins(),
            plugin::load(self.files.as_ref(), workspace, &files).await,
        ];
        match self.info.facts(workspace).await {
            Ok(facts) => layers.extend(custom::load(self.files.as_ref(), workspace, &facts).await),
            Err(error) => tracing::debug!(%error, "custom agents are not available"),
        }
        merge(layers)
    }
}

#[async_trait]
impl ListingSource for AgentCatalog {
    async fn listings(&self, request: &ContextRequest) -> Result<Listings, OrchestrationError> {
        let agents = self
            .definitions(&request.workspace)
            .await
            .iter()
            .map(|definition| ListingEntry {
                name: definition.agent_type.clone(),
                line: definition.listing_line(),
            })
            .collect();
        Ok(Listings {
            agents,
            ..Listings::default()
        })
    }
}

#[cfg(test)]
#[path = "context_tests.rs"]
mod tests;
