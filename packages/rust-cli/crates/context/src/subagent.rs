//! 子代理的 attachment（docs/specs/rust-cli/agents.md §4.3）。
//!
//! 与主线程同样分时点产生（修复线 F16）：
//! - 运行开始：进入子循环前的那几项——延迟工具、环境、模型、skill。这里只有这一组是子代理独有的；
//! - 每个工具轮结束：与主线程同一组当步 attachment（[`crate::PromptAssembler`] 的 `turn_group`），没有主线程独有的
//!   静默提醒，剩余 token 不重新锚定；
//! - 每一步：上下文宣布（规则文件、会话上下文、日期、署名）。Explore / Plan 不带规则文件与 git 状态（按
//!   类型名省略）。
//!
//! 各段的增量逻辑与主线程相同，只看子会话自己的历史。

use rust_cli_kernel_api::listing::Listings;
use rust_cli_kernel_api::message::{AttachmentMessage, Message};
use rust_cli_kernel_api::orchestration::ContextRequest;

use crate::system::SKILL_TOOL;
use crate::{PromptAssembler, announce, environment, listings};

/// 不带规则文件与 git 状态的内置 agent（也不发 nested memory，同 F16 之前）。
pub(crate) fn omits_context(agent_type: &str) -> bool {
    agent_type == "Explore" || agent_type == "Plan"
}

impl PromptAssembler {
    /// 子代理一次运行开始时的 attachment。`session` 是全部历史（skill
    /// 清单的“已宣布”跨压缩保留），`history` 是最近一次压缩边界之后的部分。续跑时按全部历史算增量，已宣布的不再发。
    pub(crate) async fn subagent_start(
        &self,
        request: &ContextRequest,
        (session, history): (&[Message], &[Message]),
        listings: &Listings,
    ) -> Vec<AttachmentMessage> {
        let mut out = Vec::new();
        out.extend(
            listings::deferred_tools(
                history,
                &listings.deferred_tools,
                listings::pending_servers(request).as_deref(),
            )
            .map(|found| self.attachment(listings::DEFERRED_TOOLS, found)),
        );
        // 环境段的主工作目录是子会话的当前目录（与工具轮的 `turn_group` 同一来源）：子代理在派生时
        // 复制父会话的 cwd，子代理的 `cd` 不保留，所以它的当前目录一直是派生时的目录
        // （tools-files.md §3.3）。缺陷修复（F19）：以前这里与工具轮都按项目根——父会话 `cd` 之后派的子代理，
        // 环境段写的不是它的工作目录（它的文件工具与 Bash 都在派生时的目录）。
        let facts = self.facts(request).await;
        if let Some(found) = self
            .shown_facts(request, facts.as_ref())
            .await
            .and_then(|facts| environment::announce(history, &facts))
        {
            out.push(self.attachment(environment::SOURCE, found));
        }
        out.extend(
            announce::model(history, &request.selection)
                .map(|found| self.attachment(announce::MODEL, found)),
        );
        if request.tool_names.iter().any(|tool| tool == SKILL_TOOL) {
            out.extend(
                listings::skills(session, &listings.skills)
                    .map(|found| self.attachment(listings::SKILLS, found)),
            );
        }
        out
    }
}

#[cfg(test)]
#[path = "subagent_tests.rs"]
mod tests;
