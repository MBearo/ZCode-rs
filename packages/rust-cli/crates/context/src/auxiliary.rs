//! 摘要请求（`QuerySource::Compact`）的宣布（docs/specs/rust-cli/context.md §4.1）。
//!
//! 上下文宣布对 auxiliary 来源（摘要请求）
//! 也跑：规则文件、会话上下文、日期只在这次请求的消息里还没有时宣布。压缩裁掉开头时它们随第一组被裁掉，
//! 摘要请求就重新带上它们（例如 `Today's date is …`）。

use rust_cli_kernel_api::message::{AttachmentMessage, Message};
use rust_cli_kernel_api::orchestration::ContextRequest;

use crate::render::{attachments, since_boundary};
use crate::{PromptAssembler, announce, instructions, session_context};

impl PromptAssembler {
    /// 顺序同上下文宣布：规则文件、会话上下文、日期。
    ///
    /// 缺陷修复（L25）：原来摘要请求一律不宣布，裁掉开头后摘要请求里没有日期；应补上消息里
    /// 没有的宣布。规则文件直接读、不进读取缓存：缓存按主线程的压缩边界失效，摘要请求的消息里没有边界。
    pub(crate) async fn auxiliary_announcements(
        &self,
        request: &ContextRequest,
        history: &[Message],
    ) -> Vec<AttachmentMessage> {
        let window = since_boundary(history);
        let missing = |source: &str| attachments(window, source).next().is_none();
        let mut out = Vec::new();
        if let Ok(facts) = self.ports.env.facts(&request.workspace).await {
            if missing(instructions::SOURCE) {
                let files = self.load_rule_files(request, &facts).await;
                out.extend(
                    instructions::announce(window, &files, instructions::Reason::SessionStart)
                        .map(|found| self.attachment(instructions::SOURCE, found)),
                );
            }
            if missing(session_context::SOURCE) {
                let env = self.ports.env.as_ref();
                out.extend(
                    session_context::announce(env, &request.workspace, &facts, window)
                        .await
                        .map(|found| self.attachment(session_context::SOURCE, found)),
                );
            }
        }
        if missing(announce::DATE) {
            out.extend(
                announce::date(window, &self.ports.clock.today())
                    .map(|found| self.attachment(announce::DATE, found)),
            );
        }
        out
    }
}
