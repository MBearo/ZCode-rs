//! 文件与搜索的线上类型（kernel-api `env`、`env_files`）。

use serde::{Deserialize, Serialize};

use crate::Blob;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    File,
    Directory,
    Other,
}

/// 条件写的前置条件（kernel-api `WritePrecondition`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "version", rename_all = "snake_case")]
pub enum WritePrecondition {
    Version(String),
    Absent,
    Any,
}

/// 整份文件的内容与版本（kernel-api `FileContent`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileContent {
    pub bytes: Blob,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStat {
    pub kind: FileKind,
    pub size: u64,
    pub mtime_ms: u64,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirEntry {
    pub name: String,
    pub kind: FileKind,
}

/// 按行读文本（kernel-api `ReadText`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadText {
    pub path: String,
    pub start_line: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_lines: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TextRead {
    Text { slice: TextSlice },
    Directory,
    TooLarge { size: u64 },
}

/// 选中的行（kernel-api `TextSlice`）：文字是 UTF-8，放在二进制段。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextSlice {
    pub content: Blob,
    pub line_count: u64,
    pub total_lines: u64,
    pub total_bytes: u64,
    pub read_bytes: u64,
    pub mtime_ms: u64,
    pub version: String,
}

/// 条件写整份文件（kernel-api `PutFile`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PutFile {
    pub path: String,
    pub bytes: Blob,
    pub precondition: WritePrecondition,
    #[serde(default)]
    pub create_parents: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileWritten {
    pub version: String,
    pub mtime_ms: u64,
    pub created: bool,
}

/// 按文件名模式找文件（kernel-api `GlobFiles`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlobFiles {
    pub root: String,
    pub pattern: String,
    pub limit: u64,
    pub hidden: bool,
    pub respect_ignore: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlobMatches {
    pub files: Vec<String>,
    pub total: u64,
}

/// 内容搜索（kernel-api `GrepSearch`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrepSearch {
    pub root: String,
    pub pattern: String,
    #[serde(default)]
    pub case_insensitive: bool,
    #[serde(default)]
    pub multiline: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub globs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_type: Option<String>,
    #[serde(default)]
    pub hidden: bool,
    pub mode: GrepMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GrepMode {
    FilesWithMatches,
    Count,
    Content {
        #[serde(default)]
        before: u64,
        #[serde(default)]
        after: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GrepOutcome {
    Files { files: Vec<GrepFileHit> },
    Counts { counts: Vec<GrepCount> },
    Content { files: Vec<GrepFileLines> },
    Rejected { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrepFileHit {
    pub path: String,
    pub mtime_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrepCount {
    pub path: String,
    pub count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrepFileLines {
    pub path: String,
    pub lines: Vec<GrepLine>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_offset: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrepLine {
    pub kind: GrepLineKind,
    pub line_number: u64,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub submatches: Vec<GrepSubmatch>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrepLineKind {
    Match,
    Context,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrepSubmatch {
    pub start: u64,
    pub text: String,
}

/// 渲染 PDF 的几页（kernel-api `RenderPdfPages`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderPdfPages {
    pub path: String,
    pub first_page: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_page: Option<u64>,
}

/// 渲染的结果（kernel-api `PdfRender`）：每页的 JPEG 在二进制段。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PdfRender {
    Unavailable,
    Failed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        stderr: String,
    },
    Rendered {
        pages: Vec<Blob>,
    },
}
