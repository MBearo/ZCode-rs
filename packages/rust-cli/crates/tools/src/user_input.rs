//! 用户输入能力的实现（[`UserInputs`]，docs/specs/rust-cli/tui-input.md §1.1、§1.3、§2.2）：`@` 提及经 Read 工具读
//! 文件（PDF 经执行环境直接检查），bash 模式经 Bash 工具执行。两个工具都取自主线程的基础注册表，readFileState 与
//! shell 状态因此与模型的调用共用。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::env::{EnvPath, ReadFile};
use rust_cli_kernel_api::env_files::{FileKind, ListDir};
use rust_cli_kernel_api::env_path;
use rust_cli_kernel_api::message::ToolResultContent;
use rust_cli_kernel_api::orchestration::{PermissionGate, ToolRegistry};
use rust_cli_kernel_api::permission::PermissionDecision;
use rust_cli_kernel_api::tool::{Tool, ToolProgress, ToolUseContext};
use rust_cli_kernel_api::user_input::{BashRun, InputAttachment, UserInputs};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::common::{display_path, format_bytes, resolve, stat, style};
use crate::mentions::{self, Mention};
use crate::read_support::{IMAGES, MAX_BYTES, has_binary_extension};

pub struct UserInputTools {
    read: Option<Arc<dyn Tool>>,
    /// bash 模式自己的 Bash 工具实例：不套 sandbox（用户输入的命令），与模型的 Bash 共用执行环境里的 shell 状态。
    bash: crate::bash::BashTool,
    gate: Arc<dyn PermissionGate>,
}

impl UserInputTools {
    /// `registry` 是主线程的基础工具注册表；`gate` 判断提及的文件是否被规则拒绝。
    pub fn new(registry: &dyn ToolRegistry, gate: Arc<dyn PermissionGate>) -> Self {
        Self {
            read: registry.find(crate::read::NAME),
            bash: crate::bash::BashTool::new(),
            gate,
        }
    }

    /// bash 模式的超时与模型的 Bash 相同（`BASH_*_TIMEOUT_MS`，cli-args.md §9.3，修复线 F42）。
    #[must_use]
    pub fn with_bash_timeouts(mut self, timeouts: crate::BashTimeouts) -> Self {
        self.bash = self.bash.with_timeouts(timeouts);
        self
    }

    async fn denied(&self, read: &dyn Tool, input: &Value, context: &ToolUseContext) -> bool {
        matches!(
            self.gate.check_rules(read, input, context).await,
            Some(PermissionDecision::Deny { .. })
        )
    }

    /// 一个提及 → 至多一个 attachment（tui-input.md §1.1 的表）。
    async fn mention(
        &self,
        read: &dyn Tool,
        mention: Mention,
        context: &ToolUseContext,
    ) -> Option<InputAttachment> {
        let root = context.workspace.root.as_str();
        let path = resolve(context, root, &mention.filename).await.ok()?;
        let display = display_path(style(context), root, &path);
        let mut input = json!({ "file_path": path });
        if let Some(offset) = mention.offset {
            input["offset"] = offset.into();
        }
        if let Some(limit) = mention.limit {
            input["limit"] = limit.into();
        }
        if self.denied(read, &input, context).await {
            return None;
        }
        let found = stat(context, &path).await.ok()??;
        match found.kind {
            FileKind::Directory => {
                let request = ListDir {
                    path: EnvPath::new(&path),
                };
                let entries = context
                    .machine
                    .files()
                    .list_dir(&context.workspace, request)
                    .await
                    .ok()?;
                let names = entries.into_iter().map(|entry| entry.name).collect();
                let names = mentions::directory_names(names);
                return Some(mentions::directory(&path, &display, &names));
            }
            FileKind::Other => return None,
            FileKind::File => {}
        }
        let extension = env_path::extension(&path);
        if extension == ".pdf" {
            return self.pdf(&path, &display, found.size, context).await;
        }
        // 太大的文件不读（与行号无关）。只有能按行分段读的文件给引用（
        // 有扩展名、不是二进制扩展名、不是 notebook）；图片、notebook、没有扩展名的不附（L36 修正，原来都给引用）。
        if found.size > MAX_BYTES {
            let by_lines =
                !extension.is_empty() && !has_binary_extension(&path) && extension != ".ipynb";
            return by_lines.then(|| mentions::too_large(&path, &format_bytes(found.size)));
        }
        // Read 的进度不显示（提及在发送时读完）。
        let (progress, _) = mpsc::channel(1);
        let output = read.call(input, context.clone(), progress).await.ok()?;
        let content = output.content.as_slice();
        // 读过且没有变化：`already_read_file` 渲染为空（notebook 相同）。
        if let [ToolResultContent::Text { text }] = content
            && text == crate::texts::FILE_UNCHANGED
        {
            return None;
        }
        if extension == ".ipynb" {
            return Some(mentions::notebook(&path, &display, content));
        }
        if IMAGES.contains(&extension.as_str()) {
            return Some(mentions::media(&path, &display, content));
        }
        let [ToolResultContent::Text { text }] = content else {
            return None;
        };
        Some(mentions::file(&path, &display, text))
    }

    /// 提及 PDF（tui-input.md §1.3）：本工程的 Read 不读 PDF，这里经执行环境做与 Read 整读
    /// 同样的检查。估计超过 10 页给引用；否则空文件与没有 `%PDF-` 开头的跳过（Read 会报错）。不写 readFileState。
    async fn pdf(
        &self,
        path: &str,
        display: &str,
        size: u64,
        context: &ToolUseContext,
    ) -> Option<InputAttachment> {
        let size_text = format_bytes(size);
        if size.div_ceil(mentions::PDF_PAGE_BYTES) > mentions::PDF_INLINE_PAGES {
            return Some(mentions::pdf_reference(path, display, size, &size_text));
        }
        // 走到这里的不超过 1,024,000 字节（20MB 上限碰不到），整个读出来只看开头。
        let request = ReadFile {
            path: EnvPath::new(path),
        };
        let file = context
            .machine
            .files()
            .read_file(&context.workspace, request)
            .await
            .ok()?;
        if !file.bytes.starts_with(b"%PDF-") {
            return None;
        }
        Some(mentions::pdf(path, display, &size_text))
    }
}

#[async_trait]
impl UserInputs for UserInputTools {
    async fn mentions(&self, input: &str, context: ToolUseContext) -> Vec<InputAttachment> {
        let Some(read) = self.read.clone() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for raw in mentions::extract(input) {
            if context.cancel.is_cancelled() {
                break;
            }
            let mention = mentions::parse_lines(&raw);
            if let Some(attachment) = self.mention(read.as_ref(), mention, &context).await {
                out.push(attachment);
            }
        }
        out
    }

    async fn bash(
        &self,
        command: &str,
        context: ToolUseContext,
        progress: mpsc::Sender<ToolProgress>,
    ) -> BashRun {
        crate::bash_mode::run(&self.bash, command, context, progress).await
    }
}

#[cfg(test)]
#[path = "user_input_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "user_input_media_tests.rs"]
mod media_tests;
