//! `@` 提及图片、PDF、notebook（docs/specs/rust-cli/tui-input.md §1.3）。

use std::sync::Mutex;

use async_trait::async_trait;
use rust_cli_kernel_api::env::{EnvFacts, EnvPath};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::message::{ATTACHMENT_BLOCKS, ContentBlock, ImageSource};
use rust_cli_kernel_api::permission::PermissionRule;
use rust_cli_kernel_api::testing::{MemoryEnvironment, ScriptedInteractions};
use rust_cli_kernel_api::tool::{PromptContext, ToolError, ToolOutput};

use super::*;
use crate::test_more::Ctx;

const PNG: &str = "iVBORw0KGgo=";

fn png() -> ToolResultContent {
    ToolResultContent::Image {
        source: ImageSource::Base64 {
            media_type: "image/png".to_owned(),
            data: PNG.to_owned(),
        },
    }
}

fn said(text: &str) -> ToolResultContent {
    ToolResultContent::Text {
        text: text.to_owned(),
    }
}

/// 按文件名给出 Read 结果的替身（图片、notebook 的几种形状），记下被读的路径。
#[derive(Default)]
struct MediaRead {
    paths: Mutex<Vec<String>>,
}

#[async_trait]
impl Tool for MediaRead {
    fn name(&self) -> &str {
        "Read"
    }
    fn input_schema(&self) -> Value {
        json!({})
    }
    fn description(&self, _context: &PromptContext) -> String {
        String::new()
    }
    async fn call(
        &self,
        input: Value,
        _context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let path = input["file_path"].as_str().unwrap_or_default().to_owned();
        self.paths.lock().unwrap().push(path.clone());
        let name = path.rsplit('/').next().unwrap_or_default();
        let content = match name {
            "pic.png" => vec![png()],
            "nb.ipynb" => vec![said("<cell id=\"a\">x = 1</cell id=\"a\">\n\n1\n")],
            "plot.ipynb" => vec![
                said("<cell id=\"p\">show()</cell id=\"p\">\n\n<Figure>"),
                png(),
                said("<cell id=\"q\"><cell_type>markdown</cell_type>done</cell id=\"q\">"),
            ],
            "empty.ipynb" => Vec::new(),
            "seen.ipynb" => vec![said(crate::texts::FILE_UNCHANGED)],
            _ => return Err(crate::common::failed("unexpected read".to_owned())),
        };
        Ok(ToolOutput {
            content,
            is_error: false,
            display: Value::Null,
        })
    }
}

struct AllowAll;

#[async_trait]
impl PermissionGate for AllowAll {
    async fn decide(&self, _: &dyn Tool, _: &Value, _: &ToolUseContext) -> PermissionDecision {
        PermissionDecision::Passthrough
    }
    async fn remember(&self, _: &SessionId, _: &[PermissionRule]) {}
}

const PDF: &str = "%PDF-1.4\n1 0 obj<</Type/Catalog>>endobj\n%%EOF\n";

fn context() -> ToolUseContext {
    let environment = MemoryEnvironment::new(EnvFacts {
        cwd: EnvPath::new("/Users/dev/work"),
        platform: "darwin".to_owned(),
        os_version: String::new(),
        shell: "zsh".to_owned(),
        is_git_repo: false,
        additional_directories: Vec::new(),
        home: Some(EnvPath::new("/Users/dev")),
    })
    .with_file("/Users/dev/work/pic.png", "png bytes")
    .with_file("/Users/dev/work/huge.png", &"p".repeat(300_000))
    .with_file("/Users/dev/work/doc.pdf", PDF)
    .with_file(
        "/Users/dev/work/big.pdf",
        &format!("{PDF}{}", "%".repeat(1_100_000)),
    )
    // 恰好 10 页（1,024,000 字节）时仍然读。
    .with_file(
        "/Users/dev/work/edge.pdf",
        &format!("{PDF}{}", "%".repeat(1_024_000 - PDF.len())),
    )
    .with_file("/Users/dev/work/fake.pdf", "not a pdf\n")
    .with_file("/Users/dev/work/empty.pdf", "")
    .with_file("/Users/dev/work/nb.ipynb", "{}")
    .with_file("/Users/dev/work/plot.ipynb", "{}")
    .with_file("/Users/dev/work/empty.ipynb", "{}")
    .with_file("/Users/dev/work/seen.ipynb", "{}")
    .with_file("/Users/dev/work/huge.ipynb", &"n".repeat(300_000))
    .with_file("/Users/dev/work/blob", &"b".repeat(300_000))
    .with_file("/Users/dev/work/big.zip", &"z".repeat(300_000))
    .with_file("/Users/dev/work/big.txt", &"x".repeat(300_000));
    let mut ctx = Ctx::new(Arc::new(ScriptedInteractions::new([])));
    ctx.env = Arc::new(environment);
    ctx.build()
}

fn tools(read: Arc<MediaRead>) -> UserInputTools {
    UserInputTools {
        read: Some(read),
        bash: crate::bash::BashTool::new(),
        gate: Arc::new(AllowAll),
    }
}

async fn mention(name: &str) -> (Vec<InputAttachment>, Vec<String>) {
    let read = Arc::new(MediaRead::default());
    let found = tools(read.clone())
        .mentions(&format!("look at @{name}"), context())
        .await;
    let paths = read.paths.lock().unwrap().clone();
    (found, paths)
}

fn wrapped(text: &str) -> String {
    format!("<system-reminder>\n{text}\n</system-reminder>")
}

fn call(path: &str) -> String {
    format!("Called the Read tool with the following input: {{\"file_path\":\"{path}\"}}")
}

#[tokio::test]
async fn an_image_is_a_call_followed_by_its_image_block() {
    let (found, read) = mention("pic.png").await;
    assert_eq!(read, ["/Users/dev/work/pic.png"]);
    let [attachment] = found.as_slice() else {
        panic!("one attachment: {found:?}");
    };
    assert_eq!(attachment.source, "file");
    assert_eq!(attachment.text, wrapped(&call("/Users/dev/work/pic.png")));
    assert_eq!(
        attachment.data,
        json!({
            "filename": "/Users/dev/work/pic.png",
            "displayPath": "pic.png",
            ATTACHMENT_BLOCKS: [{"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": PNG}}],
        })
    );
}

#[tokio::test]
async fn files_over_the_read_limit_get_a_reference_only_when_they_read_by_lines() {
    // 有扩展名、不是二进制扩展名（含图片）、不是 notebook 才给 `at_mention_reference`。
    for name in ["huge.png", "huge.ipynb", "blob", "big.zip"] {
        let (found, read) = mention(name).await;
        assert!(found.is_empty(), "{name}: {found:?}");
        assert!(read.is_empty(), "{name} is not read");
    }
    let (found, _) = mention("big.txt").await;
    assert_eq!(found[0].source, "at_mention_reference");
}

#[tokio::test]
async fn a_small_pdf_says_it_was_read_without_its_content() {
    let (found, read) = mention("doc.pdf").await;
    assert!(read.is_empty(), "PDFs do not go through Read");
    let [attachment] = found.as_slice() else {
        panic!("one attachment: {found:?}");
    };
    assert_eq!(attachment.source, "file");
    assert_eq!(
        attachment.text,
        wrapped(&format!(
            "{}\nResult of calling the Read tool:\nPDF file read: /Users/dev/work/doc.pdf ({} bytes)",
            call("/Users/dev/work/doc.pdf"),
            PDF.len()
        ))
    );
    assert_eq!(
        attachment.data,
        json!({"filename": "/Users/dev/work/doc.pdf", "displayPath": "doc.pdf"})
    );
    let (found, _) = mention("edge.pdf").await;
    assert_eq!(found[0].source, "file");
}

#[tokio::test]
async fn a_long_pdf_is_a_reference_with_an_unknown_page_count() {
    let (found, _) = mention("big.pdf").await;
    let [attachment] = found.as_slice() else {
        panic!("one attachment: {found:?}");
    };
    let size = PDF.len() + 1_100_000;
    assert_eq!(attachment.source, "pdf_reference");
    assert_eq!(
        attachment.text,
        wrapped(
            "PDF file: /Users/dev/work/big.pdf (page count unknown, 1MB). It was not attached because it may be too long. Use the Read tool with the pages parameter to read specific page ranges (e.g., pages: \"1-5\"). Start by reading the first few pages to understand the structure, then read more as needed. Maximum 20 pages per request."
        )
    );
    assert_eq!(
        attachment.data,
        json!({"filename": "/Users/dev/work/big.pdf", "pageCount": null, "fileSize": size, "displayPath": "big.pdf"})
    );
}

#[tokio::test]
async fn files_that_are_not_pdfs_are_skipped() {
    for name in ["fake.pdf", "empty.pdf"] {
        let (found, _) = mention(name).await;
        assert!(found.is_empty(), "{name}: {found:?}");
    }
}

#[tokio::test]
async fn a_text_only_notebook_is_the_json_of_its_blocks() {
    let (found, _) = mention("nb.ipynb").await;
    assert_eq!(
        found[0].text,
        wrapped(&format!(
            "{}\nResult of calling the Read tool:\n[{{\"text\":\"<cell id=\\\"a\\\">x = 1</cell id=\\\"a\\\">\\n\\n1\\n\",\"type\":\"text\"}}]",
            call("/Users/dev/work/nb.ipynb")
        ))
    );
    assert_eq!(
        found[0].data,
        json!({"filename": "/Users/dev/work/nb.ipynb", "displayPath": "nb.ipynb"})
    );
    let (found, _) = mention("empty.ipynb").await;
    assert!(
        found[0]
            .text
            .ends_with("Result of calling the Read tool:\n[]\n</system-reminder>")
    );
}

#[tokio::test]
async fn a_notebook_with_images_keeps_its_blocks_with_each_text_wrapped() {
    let (found, _) = mention("plot.ipynb").await;
    let [attachment] = found.as_slice() else {
        panic!("one attachment: {found:?}");
    };
    assert_eq!(
        attachment.text,
        wrapped(&call("/Users/dev/work/plot.ipynb"))
    );
    let blocks: Vec<ContentBlock> =
        serde_json::from_value(attachment.data[ATTACHMENT_BLOCKS].clone()).unwrap();
    let image = ContentBlock::Image {
        source: ImageSource::Base64 {
            media_type: "image/png".to_owned(),
            data: PNG.to_owned(),
        },
    };
    assert_eq!(
        blocks,
        [
            ContentBlock::Text {
                text: wrapped("<cell id=\"p\">show()</cell id=\"p\">\n\n<Figure>")
            },
            image,
            ContentBlock::Text {
                text: wrapped("<cell id=\"q\"><cell_type>markdown</cell_type>done</cell id=\"q\">")
            },
        ]
    );
}

#[tokio::test]
async fn an_unchanged_notebook_is_not_attached_again() {
    let (found, read) = mention("seen.ipynb").await;
    assert_eq!(read.len(), 1);
    assert!(found.is_empty());
}
