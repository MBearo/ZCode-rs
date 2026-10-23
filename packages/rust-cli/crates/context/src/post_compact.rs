//! 压缩附件里的上下文（docs/specs/rust-cli/compact.md §3.4）。
//!
//! 压缩收尾时生成附件：重读的文件之后是计划模式提醒，再是延迟工具、agent、MCP instructions 的清单增量（按保留段
//! 算），然后是环境段与模型身份（保留段里没有时从压缩前带过来）。这里用与当步
//! attachment 相同的增量函数生成，compact 经 kernel-api [`PostCompactContext`] 调用（只在主线程的压缩里调，由
//! compact 决定）。

use async_trait::async_trait;
use rust_cli_kernel_api::compaction::PostCompactContext;
use rust_cli_kernel_api::message::{AttachmentMessage, Message};
use rust_cli_kernel_api::orchestration::{ContextRequest, OrchestrationError};

use crate::system::AGENT_TOOL;
use crate::{PromptAssembler, announce, environment, listings, mcp};

#[async_trait]
impl PostCompactContext for PromptAssembler {
    /// 缺陷修复（L25）：原来压缩不生成清单，下一步由 context 宣布，排在环境段之后；清单应在
    /// 压缩附件里、环境段之前。生成在这里、提交在边界之后，下一步的增量看到它们就不再宣布。
    async fn post_compact(
        &self,
        request: ContextRequest,
        kept: &[Message],
    ) -> Result<Vec<AttachmentMessage>, OrchestrationError> {
        let current = self.ports.listings.listings_for(&request, kept).await?;
        let instructions = self.ports.listings.mcp_instructions(&request).await?;
        let mut out = Vec::new();
        out.extend(self.post_compact_plan_mode(&request).await);
        out.extend(
            listings::deferred_tools(
                kept,
                &current.deferred_tools,
                listings::pending_servers(&request).as_deref(),
            )
            .map(|found| self.attachment(listings::DEFERRED_TOOLS, found)),
        );
        if request.tool_names.iter().any(|tool| tool == AGENT_TOOL) {
            out.extend(
                listings::agents(kept, &current.agents)
                    .map(|found| self.attachment(listings::AGENTS, found)),
            );
        }
        out.extend(
            mcp::announce(kept, &instructions).map(|found| self.attachment(mcp::SOURCE, found)),
        );
        // F10：环境段与模型身份以前由压缩后下一步的上下文宣布；当步 attachment 改在输入与工具轮之后产生后，
        // 放进压缩附件，排在清单之后。
        let facts = self.facts(&request).await;
        if let Some(shown) = self.shown_facts(&request, facts.as_ref()).await {
            out.extend(
                environment::announce(kept, &shown)
                    .map(|found| self.attachment(environment::SOURCE, found)),
            );
        }
        out.extend(
            announce::model(kept, &request.selection)
                .map(|found| self.attachment(announce::MODEL, found)),
        );
        Ok(out)
    }
}
