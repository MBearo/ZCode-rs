//! Read 读 PDF 与 `pages`（docs/specs/rust-cli/tools-files.md §1.5，修复线 F24）。修前 PDF 走文本分支（有损解码成乱码），
//! `pages` 被忽略；执行环境的两项 PDF
//! 操作由替身 [`PdfEnv`] 回答（真实的 pdfinfo / pdftoppm 调用见 env-local 的 `pdf_tests.rs`）。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::future::BoxFuture;
use futures_util::stream::BoxStream;
use rust_cli_kernel_api::env::{
    CommandEvent, EnvError, EnvFacts, EnvPath, Environment, FileContent, OperationContext, Outcome,
    StartCommand, WorkspaceRef, WriteFile, WriteReceipt,
};
use rust_cli_kernel_api::env_files::{DirEntry, FileStat, ListDir, PathFacts, StatFile};
use rust_cli_kernel_api::env_pdf::{PdfPageCount, PdfRender, RenderPdfPages};
use rust_cli_kernel_api::env_shell::ShellState;
use rust_cli_kernel_api::ids::OperationId;
use rust_cli_kernel_api::message::ImageSource;
use rust_cli_kernel_api::model::{ModelError, ModelEvent, ModelPort, ModelRequest, ModelSelection};
use rust_cli_kernel_api::orchestration::PermissionGate;
use rust_cli_kernel_api::permission::{PermissionDecision, PermissionRule};
use rust_cli_kernel_api::provider::ModelCapabilities;
use rust_cli_kernel_api::testing::{MemoryEnvironment, ScriptedInteractions};
use tokio_util::sync::CancellationToken;

use super::*;
use crate::test_more::Ctx;
use crate::web_fetch::SideModel;

/// 一页的最小 PDF（夹具自己写，不是真实文档）。
const PDF: &str = "%PDF-1.4\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 200]>>endobj\ntrailer<</Root 1 0 R>>\n%%EOF\n";
const DOC: &str = "/Users/dev/work/doc.pdf";

/// 两项 PDF 操作按脚本回答、其余交给内存执行环境的替身。
struct PdfEnv {
    inner: MemoryEnvironment,
    /// `pdf_page_count` 的答复；None 时报 `Unsupported`（没有这项操作的执行环境）。
    pages: Option<Option<u64>>,
    /// `render_pdf_pages` 的答复；None 时报 `Unsupported`。
    render: Option<PdfRender>,
    counted: Mutex<Vec<PdfPageCount>>,
    rendered: Mutex<Vec<RenderPdfPages>>,
}

impl PdfEnv {
    fn new(files: &[(&str, &str)]) -> Self {
        let mut inner = MemoryEnvironment::new(EnvFacts {
            cwd: EnvPath::new("/Users/dev/work"),
            platform: "darwin".to_owned(),
            os_version: String::new(),
            shell: "zsh".to_owned(),
            is_git_repo: false,
            additional_directories: Vec::new(),
            home: Some(EnvPath::new("/Users/dev")),
        });
        for (path, text) in files {
            inner = inner.with_file(path, text);
        }
        Self {
            inner,
            pages: Some(None),
            render: None,
            counted: Mutex::default(),
            rendered: Mutex::default(),
        }
    }

    fn with_pages(mut self, pages: Option<u64>) -> Self {
        self.pages = Some(pages);
        self
    }

    fn with_render(mut self, render: PdfRender) -> Self {
        self.render = Some(render);
        self
    }

    fn rendered(&self) -> Vec<RenderPdfPages> {
        self.rendered.lock().unwrap().clone()
    }
}

fn unsupported(what: &str) -> EnvError {
    EnvError::Unsupported {
        what: what.to_owned(),
    }
}

#[async_trait]
impl Environment for PdfEnv {
    async fn facts(&self, workspace: &WorkspaceRef) -> Result<EnvFacts, EnvError> {
        self.inner.facts(workspace).await
    }
    async fn read_file(
        &self,
        workspace: &WorkspaceRef,
        request: rust_cli_kernel_api::env::ReadFile,
    ) -> Result<FileContent, EnvError> {
        self.inner.read_file(workspace, request).await
    }
    async fn write_file(
        &self,
        workspace: &WorkspaceRef,
        context: OperationContext,
        request: WriteFile,
    ) -> Result<WriteReceipt, EnvError> {
        self.inner.write_file(workspace, context, request).await
    }
    async fn start_command(
        &self,
        workspace: &WorkspaceRef,
        context: OperationContext,
        request: StartCommand,
    ) -> Result<(), EnvError> {
        self.inner.start_command(workspace, context, request).await
    }
    fn watch_command(
        &self,
        operation: OperationId,
        cursor: u64,
    ) -> BoxStream<'static, Result<CommandEvent, EnvError>> {
        self.inner.watch_command(operation, cursor)
    }
    async fn cancel(&self, operation: OperationId) -> Result<(), EnvError> {
        self.inner.cancel(operation).await
    }
    async fn operation_status(&self, operation: OperationId) -> Result<Outcome, EnvError> {
        self.inner.operation_status(operation).await
    }
    async fn close_session(&self, session: &SessionId, forget: bool) -> Result<(), EnvError> {
        self.inner.close_session(session, forget).await
    }
    async fn path_facts(&self, workspace: &WorkspaceRef) -> Result<PathFacts, EnvError> {
        self.inner.path_facts(workspace).await
    }
    async fn stat(
        &self,
        workspace: &WorkspaceRef,
        request: StatFile,
    ) -> Result<Option<FileStat>, EnvError> {
        self.inner.stat(workspace, request).await
    }
    async fn list_dir(
        &self,
        workspace: &WorkspaceRef,
        request: ListDir,
    ) -> Result<Vec<DirEntry>, EnvError> {
        self.inner.list_dir(workspace, request).await
    }
    async fn read_text(
        &self,
        workspace: &WorkspaceRef,
        request: rust_cli_kernel_api::env_files::ReadText,
    ) -> Result<rust_cli_kernel_api::env_files::TextRead, EnvError> {
        self.inner.read_text(workspace, request).await
    }
    async fn shell_state(
        &self,
        workspace: &WorkspaceRef,
        session: &SessionId,
    ) -> Result<ShellState, EnvError> {
        self.inner.shell_state(workspace, session).await
    }
    async fn pdf_page_count(
        &self,
        _workspace: &WorkspaceRef,
        request: PdfPageCount,
    ) -> Result<Option<u64>, EnvError> {
        self.counted.lock().unwrap().push(request);
        self.pages.ok_or_else(|| unsupported("pdf_page_count"))
    }
    async fn render_pdf_pages(
        &self,
        _workspace: &WorkspaceRef,
        request: RenderPdfPages,
    ) -> Result<PdfRender, EnvError> {
        self.rendered.lock().unwrap().push(request);
        self.render
            .clone()
            .ok_or_else(|| unsupported("render_pdf_pages"))
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

/// 模型端口替身：只回答能力（`supports_pdf`；None 是答不出）。
struct Capable(Option<bool>);

impl ModelPort for Capable {
    fn stream(
        &self,
        _request: ModelRequest,
        _cancel: CancellationToken,
    ) -> BoxStream<'static, Result<ModelEvent, ModelError>> {
        futures_util::stream::empty().boxed()
    }
    fn capabilities(
        &self,
        _selection: &ModelSelection,
    ) -> BoxFuture<'static, Option<ModelCapabilities>> {
        let pdf = self.0;
        Box::pin(async move {
            pdf.map(|supports_pdf| ModelCapabilities {
                supports_pdf,
                ..ModelCapabilities::default()
            })
        })
    }
}

fn files() -> FileTools {
    FileTools::new(Arc::default(), Arc::new(AllowAll))
}

fn tool() -> ReadTool {
    ReadTool::new(files())
}

fn tool_on(pdf: Option<bool>) -> ReadTool {
    ReadTool::new(files().with_model(SideModel {
        model: Arc::new(Capable(pdf)),
        selection: rust_cli_kernel_api::testing::selection(),
    }))
}

fn context(env: &Arc<PdfEnv>) -> ToolUseContext {
    let mut ctx = Ctx::new(Arc::new(ScriptedInteractions::default()));
    ctx.env = env.clone();
    ctx.build()
}

async fn call(tool: &ReadTool, env: &Arc<PdfEnv>, input: Value) -> Result<ToolOutput, String> {
    let (progress, _) = mpsc::channel(1);
    tool.call(input, context(env), progress)
        .await
        .map_err(|error| error.to_string())
}

async fn check(tool: &ReadTool, env: &Arc<PdfEnv>, input: Value) -> Validation {
    tool.validate_input(&input, &context(env)).await
}

fn text(text: &str) -> ToolResultContent {
    ToolResultContent::Text {
        text: text.to_owned(),
    }
}

fn jpeg(page: u8) -> Vec<u8> {
    vec![0xff, 0xd8, 0xff, 0xe0, page]
}

fn image(page: u8) -> ToolResultContent {
    ToolResultContent::Image {
        source: ImageSource::Base64 {
            media_type: "image/jpeg".to_owned(),
            data: base64::encode(&jpeg(page)),
        },
    }
}

#[tokio::test]
async fn a_whole_pdf_is_a_line_and_a_document_block() {
    let env = Arc::new(PdfEnv::new(&[(DOC, PDF)]));
    let output = call(&tool(), &env, json!({"file_path": DOC}))
        .await
        .unwrap();
    // `pdf` 的结果：说明一行，再是 document 块。
    assert_eq!(
        output.content,
        vec![
            text(&format!("PDF file read: {DOC} ({} bytes)", PDF.len())),
            ToolResultContent::Document {
                source: ImageSource::Base64 {
                    media_type: "application/pdf".to_owned(),
                    data: base64::encode(PDF.as_bytes()),
                },
            },
        ]
    );
    assert!(!output.is_error);
    // 先经 pdfinfo 问页数；PDF 不写 readFileState，下次照样重读。
    assert_eq!(
        env.counted.lock().unwrap().clone(),
        vec![PdfPageCount {
            path: EnvPath::new(DOC)
        }]
    );
    let again = call(&tool(), &env, json!({"file_path": DOC}))
        .await
        .unwrap();
    assert_eq!(again.content, output.content);
}

#[tokio::test]
async fn a_page_range_comes_back_as_images() {
    let rendered = PdfRender::Rendered {
        pages: vec![jpeg(1), jpeg(2)],
    };
    let env = Arc::new(PdfEnv::new(&[(DOC, PDF)]).with_render(rendered));
    let input = json!({"file_path": DOC, "pages": "1-2"});
    let output = call(&tool(), &env, input).await.unwrap();
    assert_eq!(
        output.content,
        vec![
            text(&format!(
                "PDF pages extracted: 2 page(s) from {DOC} ({} bytes)",
                PDF.len()
            )),
            image(1),
            image(2),
        ]
    );
    assert_eq!(
        env.rendered(),
        vec![RenderPdfPages {
            path: EnvPath::new(DOC),
            first_page: 1,
            last_page: Some(2),
        }]
    );
    // 取页不问页数（pages 分支不跑 pdfinfo）。
    assert!(env.counted.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_range_past_the_end_names_the_page_count() {
    // poppler 的原文（`pdftoppm -f 2 -l 3` 读一页的文档）。
    let failed = PdfRender::Failed {
        exit_code: Some(99),
        stderr: "Wrong page range given: the first page (2) can not be after the last page (1).\n"
            .to_owned(),
    };
    let env = Arc::new(PdfEnv::new(&[(DOC, PDF)]).with_render(failed));
    let error = call(&tool(), &env, json!({"file_path": DOC, "pages": "2-3"}))
        .await
        .unwrap_err();
    assert_eq!(
        error,
        "Requested pages 2-3 is outside the document (PDF has 1 page). Use a range within 1-1, maximum 20 pages per request (e.g. pages: \"1-1\")."
    );
}

#[tokio::test]
async fn without_pdftoppm_the_install_hint_is_given() {
    let hint = "pdftoppm is not installed. Install poppler-utils (e.g. `brew install poppler` or `apt-get install poppler-utils`) to enable PDF page rendering.";
    let input = json!({"file_path": DOC, "pages": "3"});
    let env = Arc::new(PdfEnv::new(&[(DOC, PDF)]).with_render(PdfRender::Unavailable));
    assert_eq!(call(&tool(), &env, input.clone()).await.unwrap_err(), hint);
    assert_eq!(env.rendered()[0].last_page, Some(3));
    // 执行环境没有这项操作（远端、测试替身）时同一句。
    let env = Arc::new(PdfEnv::new(&[(DOC, PDF)]));
    assert_eq!(call(&tool(), &env, input).await.unwrap_err(), hint);
}

#[tokio::test]
async fn page_ranges_are_checked_before_reading() {
    let env = Arc::new(PdfEnv::new(&[
        (DOC, PDF),
        ("/Users/dev/work/a.txt", "one\ntwo\n"),
    ]));
    let invalid = |pages: &str, code: i32| {
        let message = if code == 7 {
            format!(
                "Invalid pages parameter: \"{pages}\". Give one page (\"3\") or one range (\"1-5\"), not a list. To read several pages or ranges, read each one separately. Pages are 1-indexed."
            )
        } else {
            format!(
                "Page range \"{pages}\" exceeds maximum of 20 pages per request. Please use a smaller range."
            )
        };
        Validation::Invalid {
            message,
            error_code: Some(code),
        }
    };
    for (pages, code) in [
        ("1,3", 7),
        ("0", 7),
        ("5-2", 7),
        ("x", 7),
        ("1-21", 8),
        ("3-", 8),
    ] {
        let input = json!({"file_path": DOC, "pages": pages});
        assert_eq!(
            check(&tool(), &env, input).await,
            invalid(pages, code),
            "{pages}"
        );
    }
    for pages in ["7", " 2 - 4 ", "1-20"] {
        let input = json!({"file_path": DOC, "pages": pages});
        assert_eq!(check(&tool(), &env, input).await, Validation::Ok, "{pages}");
    }
    // 任何文件都查；不是 PDF 时通过检查的 `pages` 不起作用。
    let text_file = json!({"file_path": "/Users/dev/work/a.txt", "pages": "1,2"});
    assert_eq!(check(&tool(), &env, text_file).await, invalid("1,2", 7));
    let plain = call(&tool(), &env, json!({"file_path": "/Users/dev/work/a.txt"}))
        .await
        .unwrap();
    // 同样的 offset / limit 重读会命中“未变”，换一个文件比较。
    let env = Arc::new(PdfEnv::new(&[("/Users/dev/work/a.txt", "one\ntwo\n")]));
    let text_file = json!({"file_path": "/Users/dev/work/a.txt", "pages": "2"});
    let output = call(&tool(), &env, text_file).await.unwrap();
    assert_eq!(output.content, plain.content);
}

#[tokio::test]
async fn long_empty_and_fake_pdfs_are_refused() {
    let empty = "/Users/dev/work/empty.pdf";
    let fake = "/Users/dev/work/fake.pdf";
    let env = Arc::new(
        PdfEnv::new(&[(DOC, PDF), (empty, ""), (fake, "not a pdf\n")]).with_pages(Some(12)),
    );
    assert_eq!(
        call(&tool(), &env, json!({"file_path": DOC}))
            .await
            .unwrap_err(),
        "This PDF has 12 pages, which is too many to read at once. Use the pages parameter to read specific page ranges (e.g., pages: \"1-5\"). Maximum 20 pages per request."
    );
    let env = Arc::new(PdfEnv::new(&[(empty, ""), (fake, "not a pdf\n")]));
    assert_eq!(
        call(&tool(), &env, json!({"file_path": empty}))
            .await
            .unwrap_err(),
        format!("PDF file is empty: {empty}")
    );
    assert_eq!(
        call(&tool(), &env, json!({"file_path": empty, "pages": "1"}))
            .await
            .unwrap_err(),
        format!("PDF file is empty: {empty}")
    );
    assert_eq!(
        call(&tool(), &env, json!({"file_path": fake}))
            .await
            .unwrap_err(),
        format!("File is not a valid PDF (missing %PDF- header): {fake}")
    );
}

#[tokio::test]
async fn models_without_pdf_input_read_pages_only() {
    let rendered = PdfRender::Rendered {
        pages: vec![jpeg(1)],
    };
    let env = Arc::new(PdfEnv::new(&[(DOC, PDF)]).with_render(rendered));
    assert_eq!(
        call(&tool_on(Some(false)), &env, json!({"file_path": DOC}))
            .await
            .unwrap_err(),
        // 修复线 F51（tools-files.md §1.5）：不点名模型。
        "Reading full PDFs is not supported with this model. Use a model that supports PDF input, or use the pages parameter to read specific page ranges (e.g., pages: \"1-5\", maximum 20 pages per request). Page extraction requires poppler-utils: install with `brew install poppler` on macOS or `apt-get install poppler-utils` on Debian/Ubuntu."
    );
    // 取页不看模型能力。
    let output = call(
        &tool_on(Some(false)),
        &env,
        json!({"file_path": DOC, "pages": "1"}),
    )
    .await
    .unwrap();
    assert_eq!(output.content.len(), 2);
    // 支持 PDF、或答不出能力（测试替身、平台层还没填值）时整读。
    for pdf in [Some(true), None] {
        let output = call(&tool_on(pdf), &env, json!({"file_path": DOC}))
            .await
            .unwrap();
        assert!(matches!(
            output.content[1],
            ToolResultContent::Document { .. }
        ));
    }
}
