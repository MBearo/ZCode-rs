use super::file_write::{atomic_write, patch};
use super::tool_edit as edit;
use super::tools::{boolean, check_cancel, keys, resolve, string, uint};
use crate::contract::ToolOutput;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
use tokio::{io::AsyncReadExt, sync::Mutex};
use tokio_util::sync::CancellationToken;
const READ_BYTES: usize = 64 * 1024;
const EDIT_BYTES: u64 = 8 * 1024 * 1024;
#[derive(Default)]
pub struct FileState {
    entries: HashMap<PathBuf, Observation>,
    pub(super) views: super::read_state::Views,
}
struct Observation {
    hash: Vec<u8>,
    full: bool,
    /// The Read output was cut short (Node `isPartialView`): Edit treats it as unread.
    partial: bool,
}
pub struct FileTools<'a> {
    pub sink: Option<&'a crate::contract::EventSink>,
    pub checkpoint_root: &'a Path,
    pub cwd: &'a Path,
    pub artifacts: &'a Path,
    /// The tool children's environment (Poppler for PDF pages).
    pub env: &'a [(String, String)],
    pub state: &'a Mutex<FileState>,
    pub writes: &'a Mutex<()>,
}
impl FileTools<'_> {
    pub async fn call(
        &self,
        name: &str,
        args: &Value,
        cancel: &CancellationToken,
    ) -> Result<ToolOutput> {
        if name == "Edit" {
            // Node：先判断无改动，再判断空路径，二者都先于路径解析。
            if string(args, "old_string")? == string(args, "new_string")? {
                return Err(edit::failure(
                    edit::code::NO_CHANGE,
                    "No changes to make: old_string and new_string are exactly the same.",
                ));
            }
            if string(args, "file_path")?.is_empty() {
                return Err(edit::failure(
                    edit::code::INVALID_PATH,
                    "Tool path must not be empty",
                ));
            }
        }
        let path = resolve(self.cwd, string(args, "file_path")?)?;
        if name == "Read" {
            // Node Read 的 schema 非 strict：未知参数（包括非 PDF 文件上的 pages）被忽略。
            return self.read(&path, args, cancel).await;
        }
        keys(
            args,
            if name == "Write" {
                &["file_path", "content"]
            } else {
                &["file_path", "old_string", "new_string", "replace_all"]
            },
        )?;
        let _guard = tokio::select! { _=cancel.cancelled()=>bail!("Cancelled"), lock=self.writes.lock()=>lock };
        self.write(name, &path, args, cancel).await
    }
    async fn remember(&self, path: PathBuf, hash: Vec<u8>, full: bool, partial: bool) {
        let mut state = self.state.lock().await;
        // 读取观察缓存有界；淘汰只会要求重新 Read，不会跳过新鲜度检查。
        if state.entries.len() >= 1024 && !state.entries.contains_key(&path) {
            state.entries.clear();
        }
        state.entries.insert(
            path,
            Observation {
                hash,
                full,
                partial,
            },
        );
    }
    async fn read(
        &self,
        path: &Path,
        args: &Value,
        cancel: &CancellationToken,
    ) -> Result<ToolOutput> {
        let shown = path.to_string_lossy().into_owned();
        let path = match tokio::fs::canonicalize(path).await {
            Ok(path) => path,
            // Node Read：不存在的文件给出工作目录与相似文件名建议（抛出错误，纯文本）。
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                bail!("{}", edit::missing_message(path, self.cwd).await)
            }
            Err(e) => return Err(e.into()),
        };
        // Node：模型支持 PDF 时 .pdf 走 PDF 分支（在普通文件检查之前，文本为 Node 的处理器失败）。
        let model = super::read_pdf::Model::from_input(&args["_zcode_model_input"]["inputFormat"]);
        if model.pdf && shown.to_lowercase().ends_with(".pdf") {
            let pages = args["pages"].as_str();
            return super::read_pdf::read(
                (&path, &shown),
                pages,
                model,
                self.env,
                self.artifacts,
                cancel,
            )
            .await;
        }
        if !tokio::fs::metadata(&path).await?.is_file() {
            bail!("Read requires a regular file");
        }
        // Node：图片与视频按扩展名走媒体读取，结果为模型内容块。
        if let Some(media) = super::read_media::kind(&path) {
            return super::read_media::read(&path, media, self.artifacts, cancel).await;
        }
        let mut file = tokio::fs::File::open(&path).await?;
        let metadata = file.metadata().await?;
        if !metadata.is_file() {
            bail!("Read requires a regular file");
        }
        let start = uint(args, "offset", 1)?.max(1);
        let limit = uint(args, "limit", 2000)?;
        if limit == 0 {
            bail!("limit must be positive");
        }
        let end = start.saturating_add(limit);
        let mut buf = [0u8; 8192];
        let mut selected = Vec::new();
        let mut hash = Sha256::new();
        let mut line = 1u64;
        let mut size = 0u64;

        let mut truncated = false;
        loop {
            let n = tokio::select! { _=cancel.cancelled()=>bail!("Cancelled"), n=file.read(&mut buf)=>n? };
            if n == 0 {
                break;
            }
            if buf[..n].contains(&0) {
                bail!("Read does not support binary files");
            }
            hash.update(&buf[..n]);
            size += n as u64;
            for &byte in &buf[..n] {
                if line >= start && line < end {
                    if selected.len() < READ_BYTES {
                        selected.push(byte);
                    } else {
                        truncated = true;
                    }
                }
                if byte == b'\n' {
                    line += 1;
                }
            }
        }
        let total = if size == 0 { 0 } else { line };
        let content = match String::from_utf8(selected) {
            Ok(text) => text,
            Err(e) if truncated && e.utf8_error().error_len().is_none() => {
                String::from_utf8(e.as_bytes()[..e.utf8_error().valid_up_to()].to_vec())?
            }
            Err(_) => bail!("Read requires valid UTF-8 text"),
        };
        let mut content = content.replace("\r\n", "\n");
        if start == 1 {
            content = content.trim_start_matches('\u{feff}').to_owned();
        }
        if content.ends_with('\n') && end <= total {
            content.pop();
        }
        let count = if content.is_empty() {
            0
        } else {
            content.split('\n').count()
        };
        let full = start == 1 && !truncated && count as u64 >= total;
        self.remember(path.clone(), hash.finalize().to_vec(), full, truncated)
            .await;
        let (offset, limit) = (args["offset"].as_u64(), args["limit"].as_u64());
        let view = crate::domain::compact_ptl::ReadView {
            path: path.to_string_lossy().into_owned(),
            content: content.clone(),
            offset,
            limit,
        };
        let key = (offset.unwrap_or(1), limit);
        self.state
            .lock()
            .await
            .views
            .record(path.clone(), key, Some(view));
        let numbered = content
            .split('\n')
            .enumerate()
            .map(|(i, s)| format!("{}\t{s}", start + i as u64))
            .collect::<Vec<_>>()
            .join("\n");
        let model = if content.is_empty() {
            format!(
                "<system-reminder>{}</system-reminder>",
                if total == 0 {
                    "Warning: the file exists but the contents are empty.".to_owned()
                } else {
                    format!(
                        "Warning: the file exists but is shorter than the provided offset ({start}). The file has {total} lines."
                    )
                }
            )
        } else if truncated {
            format!(
                "<system-reminder>Partial view; use offset and limit to continue reading.</system-reminder>\n\n{numbered}"
            )
        } else {
            numbered
        };
        Ok(ToolOutput::new(
            model,
            json!({"type":"text","filePath":path,"content":content,"startLine":start,"numLines":count,"totalLines":total,"sizeBytes":size,"bytesRead":size,"truncated":truncated}),
        ))
    }
    async fn write(
        &self,
        name: &str,
        input: &Path,
        args: &Value,
        cancel: &CancellationToken,
    ) -> Result<ToolOutput> {
        let path = match tokio::fs::canonicalize(input).await {
            Ok(p) => p,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => input.to_owned(),
            Err(e) => return Err(e.into()),
        };
        let original = match tokio::fs::metadata(&path).await {
            Ok(meta) => {
                if !meta.is_file() {
                    bail!("Write/Edit requires a regular file");
                }
                if meta.len() > EDIT_BYTES {
                    bail!("File exceeds native edit budget (8 MiB)");
                }
                Some(tokio::fs::read(&path).await?)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        check_cancel(cancel)?;
        let edit = name == "Edit";
        if edit && original.is_none() && !string(args, "old_string")?.is_empty() {
            let message = edit::missing_message(&path, self.cwd).await;
            return Err(edit::failure(edit::code::FILE_NOT_EXIST, message));
        }
        let raw = std::str::from_utf8(original.as_deref().unwrap_or_default())
            .context("Write/Edit requires UTF-8 text")?;
        if raw.contains('\0') {
            bail!("Write/Edit does not support binary files");
        }
        let bom = raw.starts_with('\u{feff}');
        // Node：已有文件按多数行尾写回，新建文件为 LF。
        let crlf = edit::crlf(raw)
            || (!edit
                && original.is_none()
                && args["content"].as_str().is_some_and(|s| s.contains("\r\n")));
        let old = raw.trim_start_matches('\u{feff}').replace("\r\n", "\n");
        let replace_all = edit && boolean(args, "replace_all", false)?;
        let search = if edit {
            string(args, "old_string")?
        } else {
            ""
        };
        if edit && search.is_empty() && !crate::domain::js_string::trim(&old).is_empty() {
            return Err(edit::failure(
                edit::code::FILE_EXISTS_NO_OLD_STRING,
                "Cannot create new file - file already exists.",
            ));
        }
        if edit && !search.is_empty() && path.to_string_lossy().ends_with(".ipynb") {
            return Err(edit::failure(
                edit::code::NOTEBOOK_FILE,
                "File is a Jupyter Notebook. Use the NotebookEdit to edit this file.",
            ));
        }
        if let Some(bytes) = &original {
            let state = self.state.lock().await;
            let read = state.entries.get(&path).filter(|r| !(edit && r.partial));
            let Some(read) = read else {
                if edit {
                    return Err(edit::failure(edit::code::FILE_NOT_READ, edit::NOT_READ));
                }
                bail!("write_file_not_read: Read the file before overwriting");
            };
            if name == "Write" && !read.full {
                bail!("write_partial_read: Read the complete file before overwriting");
            }
            if read.hash != Sha256::digest(bytes).as_slice() {
                if edit {
                    return Err(edit::failure(edit::code::STALE_FILE, edit::STALE));
                }
                bail!("{name}: stale_file; file changed since Read, read it again");
            }
        }
        let mut planned = None;
        let (new, search, replacement) = if name == "Write" {
            (
                string(args, "content")?.replace("\r\n", "\n"),
                String::new(),
                String::new(),
            )
        } else {
            let search = search.replace("\r\n", "\n");
            let replacement = string(args, "new_string")?.replace("\r\n", "\n");
            if search.is_empty() {
                (replacement.clone(), search, replacement)
            } else {
                let raw_old = string(args, "old_string")?;
                let edit::Planned {
                    content,
                    actual_old,
                    actual_new,
                    strategy,
                    candidates,
                } = edit::plan(&old, &search, &replacement, replace_all, raw_old)?;
                planned = Some((strategy, candidates));
                (content, actual_old, actual_new)
            }
        };
        if new.len() as u64 > EDIT_BYTES {
            bail!("Write exceeds native edit budget (8 MiB)");
        }
        let mut bytes = if crlf {
            new.replace('\n', "\r\n").into_bytes()
        } else {
            new.as_bytes().to_vec()
        };
        if bom {
            bytes.splice(..0, [0xef, 0xbb, 0xbf]);
        }
        if let Some(sink) = self.sink {
            super::file_checkpoints::prepare(
                self.checkpoint_root,
                &path,
                name,
                original.as_deref(),
                &bytes,
                sink,
                cancel,
            )
            .await?;
        }
        atomic_write(&path, &bytes, original.as_deref(), cancel).await?;
        let path = tokio::fs::canonicalize(path).await?;
        self.remember(path.clone(), Sha256::digest(&bytes).to_vec(), true, false)
            .await;
        // Node：Edit / Write 以整文件视图覆盖，不再是压缩后的重新附带候选。
        self.state
            .lock()
            .await
            .views
            .record(path.clone(), (1, None), None);
        let (patch, additions, deletions) = patch(&old, &new);
        let mut data = if name == "Write" {
            json!({"type":if original.is_some(){"update"}else{"create"},"filePath":path,"content":new,"originalFile":original.as_ref().map(|_|&old),"structuredPatch":patch,"userModified":false})
        } else {
            json!({"filePath":path,"oldString":search,"newString":replacement,"originalFile":old,"structuredPatch":patch,"userModified":false,"replaceAll":replace_all,"matchStrategy":planned.map(|p|p.0),"matchCandidateCount":planned.map(|p|p.1)})
        };
        let mut display = json!({"kind":"file_diff","filePath":path,"additions":additions,"deletions":deletions,"structuredPatch":data["structuredPatch"]});
        if serde_json::to_vec(&display)?.len() > 32 * 1024 {
            display["structuredPatch"] = json!([]);
            display["truncated"] = true.into();
        }
        let mut content = if edit {
            // Node formatEditModelContent：使用模型给出的 file_path。
            edit::model_content(string(args, "file_path")?, replace_all)
        } else {
            format!("The file {} has been written successfully.", path.display())
        };
        if !edit && serde_json::to_vec(&data)?.len() > 64 * 1024 {
            tokio::fs::create_dir_all(self.artifacts).await?;
            let artifact = self.artifacts.join(format!("{}.json", super::id()));
            tokio::fs::write(&artifact, serde_json::to_vec(&data)?).await?;
            content.push_str(&format!(" Full change result: {}", artifact.display()));
        }
        // data is adapter-local validation output; only bounded display/model text crosses stdio.
        let mut result = ToolOutput::new(content, std::mem::take(&mut data));
        result.display = Some(display);
        Ok(result)
    }
}
