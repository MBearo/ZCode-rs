//! 压缩后重读最近的文件（docs/specs/rust-cli/compact.md §3.4）。
//!
//! 候选来自 readFileState 的复制品（压缩收尾时先复制、再清空），
//! 最近用过的在前。经工具注册表里的 Read 工具重读，所以内容与格式与模型平时看到的 Read 结果相同，执行
//! 环境照常经端口访问；重读把文件重新记进 readFileState。

use std::collections::HashSet;
use std::sync::Arc;

use rust_cli_kernel_api::clock::Ids;
use rust_cli_kernel_api::env::{OperationContext, Origin};
use rust_cli_kernel_api::ids::{Generation, OwnerEpoch, ToolUseId};
use rust_cli_kernel_api::local_services::LocalServices;
use rust_cli_kernel_api::machine::MachineEnv;
use rust_cli_kernel_api::message::{AttachmentMessage, ContentBlock, Message, ToolResultContent};
use rust_cli_kernel_api::orchestration::{CompactRequest, ToolRegistry};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::store::ArtifactStore;
use rust_cli_kernel_api::tool::ToolUseContext;
use rust_cli_prompts::compact as texts;
use rust_cli_prompts::reminder;
use rust_cli_prompts::texts::render;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::request::CollectSink;
use crate::tokens;

/// 最多重读几个文件。
const MAX_FILES: usize = 5;
/// 全部文件附件的估算上限。
const TOTAL_BUDGET: u64 = 50_000;
/// 单个文件的估算上限（重读时的读取上限）。
const FILE_BUDGET: u64 = 5_000;

const READ: &str = "Read";
pub(crate) const SOURCE: &str = "file";

/// 重读用到的端口。
pub(crate) struct Restore<'a> {
    pub tools: &'a dyn ToolRegistry,
    pub machine: MachineEnv,
    pub local: LocalServices,
    pub artifacts: Arc<dyn ArtifactStore>,
    pub ids: &'a dyn Ids,
    pub generation: Generation,
}

fn file_path(input: &Value) -> Option<String> {
    input["file_path"].as_str().map(str::to_owned)
}

/// 保留尾段里 Read 过的文件。
fn kept_reads(kept: &[&Message]) -> HashSet<String> {
    let mut out = HashSet::new();
    for message in kept {
        if let Message::Assistant(assistant) = message {
            for block in &assistant.content {
                if let ContentBlock::ToolUse { name, input, .. } = block
                    && name == READ
                    && let Some(path) = file_path(input)
                {
                    out.insert(path);
                }
            }
        }
    }
    out
}

/// 要重读的文件：readFileState 的复制品，最近的在前；去掉保留尾段里 Read 过的，最多 5 个。
pub(crate) fn candidates(recent: &[String], kept: &[&Message]) -> Vec<String> {
    let kept = kept_reads(kept);
    recent
        .iter()
        .filter(|path| !kept.contains(*path))
        .take(MAX_FILES)
        .cloned()
        .collect()
}

/// 文件附件的渲染：调用说明加结果说明。
///
/// 缺陷修复：两段原来空一行连接；折进 system 消息的文件附件里调用说明与
/// 结果说明之间只隔一个换行（`…{"file_path":…}\nResult of calling the Read tool:\n…`）。
pub(crate) fn render_file(path: &str, result: &str) -> String {
    let input = json!({ "file_path": path }).to_string();
    let call = render(texts::tool_call(), &[("tool", READ), ("input", &input)]);
    let result = render(texts::tool_result(), &[("tool", READ), ("result", result)]);
    reminder::wrap(&format!("{call}\n{result}"))
}

impl Restore<'_> {
    async fn read(
        &self,
        request: &CompactRequest,
        path: &str,
        cancel: &CancellationToken,
    ) -> Option<String> {
        let tool = self.tools.find(READ)?;
        let (progress, _updates) = tokio::sync::mpsc::channel(16);
        let context = ToolUseContext {
            session: request.session.clone(),
            run: self.ids.run(),
            tool_use: ToolUseId::new(self.ids.operation().as_str()),
            agent: request.agent.clone(),
            workspace: request.workspace.clone(),
            mode: request.mode.unwrap_or(PermissionMode::Default),
            cancel: cancel.clone(),
            machine: self.machine.clone(),
            local: self.local.clone(),
            artifacts: self.artifacts.clone(),
            interactions: Arc::new(CollectSink::default()),
            operation: OperationContext {
                environment: request.workspace.environment.clone(),
                generation: self.generation.clone(),
                owner_epoch: OwnerEpoch::LOCAL,
                operation: self.ids.operation(),
                origin: Origin::Agent {
                    session: request.session.clone(),
                    run: self.ids.run(),
                },
            },
        };
        let output = tool
            .call(json!({ "file_path": path }), context, progress)
            .await
            .ok()?;
        if output.is_error {
            return None;
        }
        let mut text = String::new();
        for item in &output.content {
            match item {
                ToolResultContent::Text { text: part } => text.push_str(part),
                // 图片、PDF 等非文本结果不重读（spec §8）。
                ToolResultContent::Image { .. }
                | ToolResultContent::Document { .. }
                | ToolResultContent::ToolReference { .. } => {
                    return None;
                }
            }
        }
        (tokens::rough(&text) <= FILE_BUDGET).then_some(text)
    }

    /// 文件附件，最近的在前；总量超出预算的跳过（按顺序累计）。`recent` 是清空前的 readFileState。
    pub(crate) async fn files(
        &self,
        request: &CompactRequest,
        recent: &[String],
        kept: &[&Message],
        cancel: &CancellationToken,
    ) -> Vec<AttachmentMessage> {
        let mut out = Vec::new();
        let mut used = 0;
        for path in candidates(recent, kept) {
            let Some(result) = self.read(request, &path, cancel).await else {
                continue;
            };
            let text = render_file(&path, &result);
            let cost = tokens::rough(&text);
            if used + cost > TOTAL_BUDGET {
                continue;
            }
            used += cost;
            out.push(AttachmentMessage {
                id: self.ids.message(),
                source: SOURCE.to_owned(),
                text,
                data: json!({ "filename": path }),
            });
        }
        out
    }
}

#[cfg(test)]
#[path = "restore_tests.rs"]
mod tests;
