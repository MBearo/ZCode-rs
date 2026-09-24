//! Read of PDF files (Node `read-pdf.ts` with the Poppler adapter): the whole
//! document as a file block, or pages rendered by `pdftoppm` as images.
//! Spec rust-m5-tools §6.
use super::read_media::{Prepared, prepare};
use crate::contract::{ToolError, ToolOutput};
use crate::domain::read_pdf::{self as pdf, code};
use crate::domain::session::StoredAttachment;
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// `pdftoppm` answered once in this process (a failed probe is not cached).
static RENDERER: AtomicBool = AtomicBool::new(false);

/// The model's input formats, as core attaches them to the call.
#[derive(Clone, Copy, Default)]
pub(super) struct Model {
    pub pdf: bool,
    pub image: bool,
}

impl Model {
    pub(super) fn from_input(input: &Value) -> Self {
        Self {
            pdf: input["supportsPdf"] == true,
            image: input["supportsImage"] == true,
        }
    }
}

fn failure(code: u32, message: impl Into<String>) -> anyhow::Error {
    ToolError::handler(code, message)
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Runs a Poppler tool with the tool environment; `None` when it cannot start
/// or times out (`timed_out` tells which).
async fn run(
    program: &str,
    args: &[&str],
    env: &[(String, String)],
    timeout_ms: u64,
    cancel: &CancellationToken,
) -> Result<std::result::Result<Run, bool>> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // 超时或取消时丢弃 future 即结束子进程。
        .kill_on_drop(true);
    let Ok(child) = command.spawn() else {
        return Ok(Err(false));
    };
    tokio::select! {
        _ = cancel.cancelled() => bail!("Cancelled"),
        _ = tokio::time::sleep(Duration::from_millis(timeout_ms)) => Ok(Err(true)),
        output = child.wait_with_output() => Ok(match output {
            Ok(output) => Ok(Run {
                code: output.status.code(),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            }),
            Err(_) => Err(false),
        }),
    }
}

async fn save(
    data: &[u8],
    mime: &str,
    source: &Path,
    artifacts: &Path,
) -> Result<StoredAttachment> {
    let extension = if mime == "application/pdf" {
        "pdf"
    } else {
        mime.rsplit('/').next().unwrap_or("bin")
    };
    let directory = artifacts.join("read-media");
    tokio::fs::create_dir_all(&directory).await?;
    let path = directory.join(format!("{}.{extension}", uuid::Uuid::new_v4()));
    tokio::fs::write(&path, data).await?;
    Ok(StoredAttachment {
        path: path.to_string_lossy().into_owned(),
        source_path: Some(source.to_string_lossy().into_owned()),
        media_type: mime.into(),
        total_bytes: data.len() as u64,
        ..Default::default()
    })
}

/// Node `readNativePdf`.
async fn native(
    path: &Path,
    shown: &str,
    size: u64,
    env: &[(String, String)],
    artifacts: &Path,
    cancel: &CancellationToken,
) -> Result<ToolOutput> {
    if size > pdf::NATIVE_MAX_BYTES {
        let limit = pdf::file_size(pdf::NATIVE_MAX_BYTES);
        return Err(failure(
            code::TOO_LARGE,
            format!("PDF file exceeds maximum allowed size of {limit}."),
        ));
    }
    let file = path.to_string_lossy();
    if let Ok(run) = run("pdfinfo", &[&file], env, pdf::INFO_TIMEOUT_MS, cancel).await?
        && run.code == Some(0)
        && let Some(pages) = pdf::page_count(&run.stdout).filter(|n| *n > pdf::NATIVE_MAX_PAGES)
    {
        return Err(failure(
            code::TOO_MANY_PAGES,
            format!(
                "This PDF has {pages} pages, which is too many to read at once. Use the pages parameter to read specific page ranges (e.g., pages: \"1-5\"). Maximum {} pages per request.",
                pdf::MAX_PAGES
            ),
        ));
    }
    let data = tokio::fs::read(path).await?;
    if !data.starts_with(b"%PDF-") {
        return Err(failure(
            code::INVALID,
            format!("File is not a valid PDF (missing %PDF- header): {shown}"),
        ));
    }
    let asset = save(&data, "application/pdf", path, artifacts).await?;
    let name = path
        .file_name()
        .map_or(String::new(), |n| n.to_string_lossy().into_owned());
    let heading = format!("PDF file read: {shown} ({})", pdf::file_size(size));
    let mut output = ToolOutput::new(
        format!("{heading}\n\n[Attached application/pdf: {name}]"),
        json!({"type": "pdf", "filePath": shown, "originalSize": size}),
    );
    output.model_content = Some(json!([{"type": "text", "text": heading},
        {"type": "_zcode_attachment", "asset": asset, "name": name, "placeholder": name}]));
    Ok(output)
}

/// Node Poppler `ensureAvailable`.
async fn renderer(env: &[(String, String)], cancel: &CancellationToken) -> Result<()> {
    if RENDERER.load(Ordering::Relaxed) {
        return Ok(());
    }
    match run("pdftoppm", &["-v"], env, pdf::PROBE_TIMEOUT_MS, cancel).await? {
        Err(true) => Err(failure(
            code::TIMEOUT,
            format!(
                "PDF page extraction availability check timed out after {}ms.",
                pdf::PROBE_TIMEOUT_MS
            ),
        )),
        Ok(run)
            if run.code == Some(0)
                || run.code.is_some_and(|c| c != 127) && !run.stderr.is_empty() =>
        {
            RENDERER.store(true, Ordering::Relaxed);
            Ok(())
        }
        _ => Err(failure(
            code::CONFIGURATION,
            "pdftoppm is not installed. Install poppler-utils (e.g. `brew install poppler` or `apt-get install poppler-utils`) to enable PDF page rendering.",
        )),
    }
}

/// Renders `first..=last` into JPEG pages in a temporary directory (always removed).
async fn render(
    path: &Path,
    (first, last): (u64, u64),
    env: &[(String, String)],
    cancel: &CancellationToken,
) -> Result<Vec<(u64, Vec<u8>)>> {
    let directory = std::env::temp_dir().join(format!("zcode-read-pdf-{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir(&directory).await.map_err(|_| {
        failure(
            code::IO,
            "Unable to create a temporary directory for PDF page extraction.",
        )
    })?;
    let result = async {
        let file = path.to_string_lossy();
        let prefix = directory.join("page");
        let (first_text, last_text) = (first.to_string(), last.to_string());
        let args = [
            "-jpeg",
            "-r",
            "100",
            "-f",
            &first_text,
            "-l",
            &last_text,
            &file,
            &prefix.to_string_lossy(),
        ];
        match run("pdftoppm", &args, env, pdf::RENDER_TIMEOUT_MS, cancel).await? {
            Err(true) => {
                return Err(failure(
                    code::TIMEOUT,
                    format!(
                        "PDF page extraction timed out after {}ms.",
                        pdf::RENDER_TIMEOUT_MS
                    ),
                ));
            }
            Err(false) => return Err(failure(code::PROCESS, "pdftoppm failed.")),
            Ok(run) if run.code != Some(0) => {
                let (code, message) =
                    pdf::render_failure((&run.stderr, &run.stdout), &file, (first, last));
                return Err(failure(code, message));
            }
            Ok(_) => {}
        }
        let mut pages = vec![];
        let mut entries = tokio::fs::read_dir(&directory).await?;
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(number) = pdf::page_number(&name) {
                pages.push((number, tokio::fs::read(entry.path()).await?));
            }
        }
        if pages.is_empty() {
            return Err(failure(
                code::INVALID,
                "pdftoppm produced no output pages. The PDF may be invalid.",
            ));
        }
        pages.sort_by_key(|(number, _)| *number);
        Ok(pages)
    }
    .await;
    let _ = tokio::fs::remove_dir_all(&directory).await;
    result
}

/// Node `readPdfPages`.
async fn pages(
    path: &Path,
    shown: &str,
    (pages, size): (&str, u64),
    env: &[(String, String)],
    artifacts: &Path,
    cancel: &CancellationToken,
) -> Result<ToolOutput> {
    if size > pdf::EXTRACT_MAX_BYTES {
        let limit = pdf::file_size(pdf::EXTRACT_MAX_BYTES);
        return Err(failure(
            code::TOO_LARGE,
            format!("PDF file exceeds maximum allowed size for text extraction ({limit})."),
        ));
    }
    if let Some(message) = pdf::pages_error(pages) {
        return Err(failure(code::INVALID, message));
    }
    let range = pdf::parse_range(pages).unwrap_or(pdf::Range {
        first: 1,
        last: Some(1),
    });
    let last = range.last.unwrap_or(range.first);
    renderer(env, cancel).await?;
    let rendered = render(path, (range.first, last), env, cancel).await?;
    let heading = format!(
        "PDF pages extracted: {} page(s) from {shown} ({})",
        rendered.len(),
        pdf::file_size(size)
    );
    let mut texts = vec![heading.clone()];
    let mut blocks = vec![json!({"type": "text", "text": heading})];
    let mut parts = vec![];
    for (number, data) in rendered {
        let prepared: Prepared = tokio::task::spawn_blocking(move || prepare(data, "image/jpeg"))
            .await?
            .map_err(anyhow::Error::msg)?;
        let asset = save(&prepared.data, prepared.mime, path, artifacts).await?;
        let placeholder = format!("PDF page {number}");
        texts.push(format!("[Attached {}: {placeholder}]", prepared.mime));
        blocks.push(json!({"type": "_zcode_attachment", "asset": asset, "name": placeholder, "placeholder": placeholder}));
        let mut info = prepared.info;
        info["pageNumber"] = number.into();
        info["mimeType"] = prepared.mime.into();
        parts.push(info);
    }
    let data = json!({"type": "parts", "filePath": shown, "numParts": parts.len(), "originalSize": size, "pages": parts});
    let mut output = ToolOutput::new(texts.join("\n\n"), data);
    output.model_content = Some(Value::Array(blocks));
    Ok(output)
}

/// Node `readPdfFile`; `shown` is the path as resolved from the input.
pub(super) async fn read(
    (path, shown): (&Path, &str),
    requested: Option<&str>,
    model: Model,
    env: &[(String, String)],
    artifacts: &Path,
    cancel: &CancellationToken,
) -> Result<ToolOutput> {
    if requested.is_some() && !model.image {
        return Err(failure(
            code::IMAGES_UNSUPPORTED,
            "The current model supports PDF input but does not support image input; remove the pages parameter.",
        ));
    }
    let metadata = tokio::fs::metadata(path).await?;
    if !metadata.is_file() {
        return Err(failure(
            code::INVALID,
            format!("Path is not a regular file: {shown}"),
        ));
    }
    if metadata.len() == 0 {
        return Err(failure(
            code::INVALID,
            format!("PDF file is empty: {shown}"),
        ));
    }
    match requested {
        None => native(path, shown, metadata.len(), env, artifacts, cancel).await,
        Some(requested) => {
            pages(
                path,
                shown,
                (requested, metadata.len()),
                env,
                artifacts,
                cancel,
            )
            .await
        }
    }
}
