//! 工作区文件与搜索操作的 DTO（docs/specs/rust-cli/tools-files.md §3）。
//!
//! 这些类型跨执行环境端口传递，只含可序列化的数据（spec §6 规则 8）：路径是 [`EnvPath`]，
//! 版本是执行侧给出的 [`FileVersion`]，时间是毫秒整数。语义部分（行号格式、截断、排序、错误文本）
//! 在内核的工具里；这里只描述“执行侧要做什么、返回什么”。

use serde::{Deserialize, Serialize};

use crate::env::{EnvPath, FileVersion, WritePrecondition};

/// 解析用户给的路径所需的执行侧事实：`~` 展开用的 home，路径风格。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathFacts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home: Option<EnvPath>,
    pub style: PathStyle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathStyle {
    Posix,
    Windows,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    File,
    Directory,
    /// 设备、FIFO、socket 等。
    Other,
}

/// 版本库目录（Grep 的排除表；文件索引 `rg --files` 的排除表同值
/// 同序）：Grep 总是排除它们，`@` 补全的文件清单跳过它们（tui-input.md §1.2）。F58：以前 tools 与 env-local 各写一份。
pub const VCS_DIRS: &[&str] = &[".git", ".svn", ".hg", ".bzr", ".jj", ".sl"];

/// 查询一个路径的元数据；不存在时端口返回 `Ok(None)`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatFile {
    pub path: EnvPath,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStat {
    pub kind: FileKind,
    pub size: u64,
    /// 修改时间，毫秒，向下取整（`Math.floor(mtimeMs)`）。
    pub mtime_ms: u64,
    pub version: FileVersion,
}

/// 列出一个目录的直接子项（不递归，不排序）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListDir {
    pub path: EnvPath,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirEntry {
    pub name: String,
    pub kind: FileKind,
}

/// 按行读取文本：UTF-8 有损解码、去掉开头的 BOM；
/// 行按 `\n` 切分，每行去掉末尾的 `\r`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadText {
    pub path: EnvPath,
    /// 从第几行开始（0 起）。
    pub start_line: u64,
    /// 最多读几行；None 表示读到文件末尾。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_lines: Option<u64>,
    /// 文件超过这个字节数时不读，返回 [`TextRead::TooLarge`]。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TextRead {
    Text(TextSlice),
    Directory,
    TooLarge { size: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextSlice {
    /// 选中的行，以 `\n` 连接。
    pub content: String,
    pub line_count: u64,
    pub total_lines: u64,
    pub total_bytes: u64,
    pub read_bytes: u64,
    pub mtime_ms: u64,
    pub version: FileVersion,
}

/// 条件写整份文件：检查前置条件与替换在执行侧一次完成（spec §6 规则 1）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PutFile {
    pub path: EnvPath,
    pub bytes: Vec<u8>,
    pub precondition: WritePrecondition,
    /// 父目录不存在时是否创建。
    #[serde(default)]
    pub create_parents: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileWritten {
    pub version: FileVersion,
    pub mtime_ms: u64,
    /// 写入前文件不存在。
    pub created: bool,
}

/// 按文件名模式找文件（Glob：`rg --files --glob <pattern> --sort=modified`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlobFiles {
    /// 搜索目录（绝对路径）。
    pub root: EnvPath,
    /// rg `--glob` 语义的模式（gitignore 风格，相对 `root`）。
    pub pattern: String,
    /// 最多返回几个；`total` 仍是全部匹配数。
    pub limit: usize,
    /// 是否包含隐藏文件（rg `--hidden`）。
    pub hidden: bool,
    /// 是否遵守 .gitignore / .ignore（false 即 rg `--no-ignore`）。
    pub respect_ignore: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlobMatches {
    /// 按修改时间升序的绝对路径（rg `--sort=modified`）。
    pub files: Vec<EnvPath>,
    pub total: usize,
}

/// 工作区文件清单（`@` 补全的索引来源；docs/specs/rust-cli/tui-input.md
/// §1.2）：遵守忽略规则、包含隐藏文件、跳过版本库目录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListFiles {
    /// 起点目录（绝对路径）；清单里的路径相对它。
    pub cwd: EnvPath,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileList {
    /// 相对 `cwd` 的文件路径（执行环境的分隔符），按路径排序。
    pub files: Vec<String>,
}

/// 内容搜索（Grep 工具，rg 语义）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrepSearch {
    /// 搜索的文件或目录（绝对路径）。
    pub root: EnvPath,
    pub pattern: String,
    #[serde(default)]
    pub case_insensitive: bool,
    /// rg `-U --multiline-dotall`。
    #[serde(default)]
    pub multiline: bool,
    /// rg `--glob`，按顺序；`!` 开头表示排除。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub globs: Vec<String>,
    /// rg `--type`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_type: Option<String>,
    /// rg `--hidden`。
    #[serde(default)]
    pub hidden: bool,
    pub mode: GrepMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GrepMode {
    /// rg `-l`。
    FilesWithMatches,
    /// rg `-c`：每个文件匹配的行数。
    Count,
    /// rg `--json`：匹配行与上下文行。
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
    Files {
        files: Vec<GrepFileHit>,
    },
    Counts {
        counts: Vec<GrepCount>,
    },
    Content {
        files: Vec<GrepFileLines>,
    },
    /// 模式、glob 或类型不合法，没有搜索（rg 退出码 2 且没有输出）。`message` 是 rg 的错误文本。
    Rejected {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrepFileHit {
    pub path: EnvPath,
    pub mtime_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrepCount {
    pub path: EnvPath,
    pub count: u64,
}

/// 一个文件里的匹配块与上下文块（rg JSON 的 begin … end）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrepFileLines {
    pub path: EnvPath,
    pub lines: Vec<GrepLine>,
    /// 文件按二进制处理时 NUL 字节的位置（rg JSON `end.binary_offset`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_offset: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrepLine {
    pub kind: GrepLineKind,
    /// 块的第一行行号（1 起）。
    pub line_number: u64,
    /// 块的原文（多行模式下可能跨行），保留行尾。
    pub text: String,
    /// 匹配的部分（rg JSON `submatches`）：`start` 是块内的字节偏移。
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

#[cfg(test)]
#[path = "env_files_tests.rs"]
mod tests;
