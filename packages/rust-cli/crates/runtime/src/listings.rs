//! 上下文清单的组合来源（docs/specs/rust-cli/context.md §2、tools-more.md §3.4）：agent 类型（L7）、
//! 延迟工具（L11a）、skill 与 MCP instructions（L9）各自提供一部分，这里按类拼起来交给 context。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::listing::{InvokedSkill, ListingEntry, ListingSource, Listings};
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::orchestration::{ContextRequest, OrchestrationError};

pub(crate) struct CombinedListings(pub(crate) Vec<Arc<dyn ListingSource>>);

#[async_trait]
impl ListingSource for CombinedListings {
    async fn listings(&self, request: &ContextRequest) -> Result<Listings, OrchestrationError> {
        self.listings_for(request, &[]).await
    }

    async fn listings_for(
        &self,
        request: &ContextRequest,
        history: &[Message],
    ) -> Result<Listings, OrchestrationError> {
        let mut out = Listings::default();
        for source in &self.0 {
            let part = source.listings_for(request, history).await?;
            out.deferred_tools.extend(part.deferred_tools);
            out.agents.extend(part.agents);
            out.skills.extend(part.skills);
        }
        Ok(out)
    }

    async fn mcp_instructions(
        &self,
        request: &ContextRequest,
    ) -> Result<Vec<ListingEntry>, OrchestrationError> {
        let mut out = Vec::new();
        for source in &self.0 {
            out.extend(source.mcp_instructions(request).await?);
        }
        Ok(out)
    }

    /// 输入中间的 skill 提及（ext.md §2.8）：各来源的名字按来源的顺序接起来（只有 ext 给出）。
    async fn skill_mentions(
        &self,
        request: &ContextRequest,
        input: &str,
    ) -> Result<Vec<String>, OrchestrationError> {
        let mut out = Vec::new();
        for source in &self.0 {
            out.extend(source.skill_mentions(request, input).await?);
        }
        Ok(out)
    }

    async fn invoked_skills(
        &self,
        request: &ContextRequest,
        history: &[Message],
    ) -> Result<Vec<InvokedSkill>, OrchestrationError> {
        let mut out = Vec::new();
        for source in &self.0 {
            out.extend(source.invoked_skills(request, history).await?);
        }
        Ok(out)
    }
}

#[cfg(test)]
#[path = "listings_tests.rs"]
mod tests;
