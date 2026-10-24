//! 规则文件的读取与遍历（context.md §11.1–§11.3）：单个文件、
//! @include 与规则目录。文件经执行环境端口读；这里不做 IO。
//!
//! 遍历用显式栈而不是递归，顺序是深度优先：先放本文件，再按出现顺序放它
//! include 的文件；规则目录按目录项的顺序，遇到子目录先走完子目录。

use std::collections::HashSet;

use rust_cli_kernel_api::env::{EnvError, EnvPath, ReadFile, WorkspaceRef};
use rust_cli_kernel_api::env_files::{FileKind, ListDir, PathStyle, StatFile};
use rust_cli_kernel_api::env_path::{extension, is_unc};
use rust_cli_kernel_api::machine::Files;

use crate::rules_frontmatter;
use crate::rules_markdown;

/// `.zcode/` 下的规则目录：managed、用户与每一层项目都有（F55：以前在两个文件里写了 8 次）。
pub(crate) const RULES_DIR: &str = "rules";

/// @include 的最大深度。
const MAX_DEPTH: usize = 5;
/// 超过这个大小的文件不读（4 MiB）。
const MAX_BYTES: u64 = 4_194_304;

/// @include 只收这些扩展名（按小写比较）；没有扩展名的文件照收。
const TEXT_EXTENSIONS: &[&str] = &[
    ".md",
    ".txt",
    ".text",
    ".json",
    ".yaml",
    ".yml",
    ".toml",
    ".xml",
    ".csv",
    ".html",
    ".htm",
    ".css",
    ".scss",
    ".sass",
    ".less",
    ".js",
    ".ts",
    ".tsx",
    ".jsx",
    ".mjs",
    ".cjs",
    ".mts",
    ".cts",
    ".py",
    ".pyi",
    ".pyw",
    ".rb",
    ".erb",
    ".rake",
    ".go",
    ".rs",
    ".java",
    ".kt",
    ".kts",
    ".scala",
    ".c",
    ".cpp",
    ".cc",
    ".cxx",
    ".h",
    ".hpp",
    ".hxx",
    ".cs",
    ".swift",
    ".sh",
    ".bash",
    ".zsh",
    ".fish",
    ".ps1",
    ".bat",
    ".cmd",
    ".env",
    ".ini",
    ".cfg",
    ".conf",
    ".config",
    ".properties",
    ".sql",
    ".graphql",
    ".gql",
    ".proto",
    ".vue",
    ".svelte",
    ".astro",
    ".ejs",
    ".hbs",
    ".pug",
    ".jade",
    ".php",
    ".pl",
    ".pm",
    ".lua",
    ".r",
    ".dart",
    ".ex",
    ".exs",
    ".erl",
    ".hrl",
    ".clj",
    ".cljs",
    ".cljc",
    ".edn",
    ".hs",
    ".lhs",
    ".elm",
    ".ml",
    ".mli",
    ".f",
    ".f90",
    ".f95",
    ".for",
    ".cmake",
    ".make",
    ".makefile",
    ".gradle",
    ".sbt",
    ".rst",
    ".adoc",
    ".asciidoc",
    ".org",
    ".tex",
    ".latex",
    ".lock",
    ".log",
    ".diff",
    ".patch",
];

/// 规则文件的类型（memory type）；记录里写成 [`Kind::name`]（`Managed` 等）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Managed,
    User,
    Project,
    Local,
    /// 自动记忆的 MEMORY.md（memory.md §4）：排在最后。
    AutoMem,
}

impl Kind {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Managed => "Managed",
            Self::User => "User",
            Self::Project => "Project",
            Self::Local => "Local",
            Self::AutoMem => "AutoMem",
        }
    }
}

/// 加载出的一个规则文件。`content` 是处理后的正文（去掉 frontmatter
/// 与块级注释），没有 trim；`globs` 有值时是条件规则；`parent` 是 include 它的文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuleFile {
    pub path: String,
    pub kind: Kind,
    pub content: String,
    pub globs: Option<Vec<String>>,
    pub parent: Option<String>,
}

/// 一次加载用到的端口与事实。`cwd` 是工作区根（original cwd）。规则文件与自动记忆都是本机的文件（remote-exec.md §3）。
pub(crate) struct Scope<'a> {
    pub files: &'a dyn Files,
    pub workspace: &'a WorkspaceRef,
    pub style: PathStyle,
    pub cwd: String,
    pub home: Option<String>,
}

/// 读取出错时跳过这个文件，不让一次加载失败。
fn skipped(path: &str, error: &EnvError) {
    if !matches!(error, EnvError::NotFound { .. }) {
        tracing::debug!(path, %error, "instruction file skipped");
    }
}

impl Scope<'_> {
    pub(crate) async fn stat_kind(&self, path: &str) -> Option<(FileKind, u64)> {
        let request = StatFile {
            path: EnvPath::new(path),
        };
        match self.files.stat(self.workspace, request).await {
            Ok(found) => found.map(|stat| (stat.kind, stat.size)),
            Err(error) => {
                skipped(path, &error);
                None
            }
        }
    }

    /// 原文（UTF-8 有损解码）。不用 `read_text`：它去掉 BOM 与行尾的 `\r`，规则文件要读原文。
    pub(crate) async fn read(&self, path: &str) -> Option<String> {
        let request = ReadFile {
            path: EnvPath::new(path),
        };
        match self.files.read_file(self.workspace, request).await {
            Ok(content) => Some(String::from_utf8_lossy(&content.bytes).into_owned()),
            Err(error) => {
                skipped(path, &error);
                None
            }
        }
    }

    async fn list(&self, dir: &str) -> Vec<(String, FileKind)> {
        let request = ListDir {
            path: EnvPath::new(dir),
        };
        match self.files.list_dir(self.workspace, request).await {
            Ok(entries) => entries
                .into_iter()
                .map(|entry| (entry.name, entry.kind))
                .collect(),
            Err(error) => {
                skipped(dir, &error);
                Vec::new()
            }
        }
    }
}

/// 解析正文：frontmatter、块级注释、@include 的原文路径。
fn parse(raw: &str) -> (String, Option<Vec<String>>, Vec<String>) {
    let (content, globs) = rules_frontmatter::split(raw);
    let has_comment = content.contains("<!--");
    // marked lexer 先把 `\r\n` / `\r` 换成 `\n`；只有去注释时正文才由 token 重拼。
    let lexed = (has_comment || content.contains('@'))
        .then(|| content.replace("\r\n", "\n").replace('\r', "\n"));
    let includes = lexed
        .as_deref()
        .map(rules_markdown::includes)
        .unwrap_or_default();
    let content = match (has_comment, lexed) {
        (true, Some(lexed)) => rules_markdown::strip_comments(&lexed),
        _ => content,
    };
    (content, globs, includes)
}

/// 一次加载里已处理的路径。
pub(crate) struct Walk<'s, 'a> {
    pub scope: &'s Scope<'a>,
    processed: HashSet<String>,
}

impl<'s, 'a> Walk<'s, 'a> {
    pub(crate) fn new(scope: &'s Scope<'a>) -> Self {
        Self {
            scope,
            processed: HashSet::new(),
        }
    }

    /// 单个文件本身（不含 include）；跳过时为 None。
    async fn one(
        &mut self,
        path: &str,
        kind: Kind,
        depth: usize,
        parent: Option<String>,
    ) -> Option<(RuleFile, Vec<String>)> {
        let style = self.scope.style;
        if is_unc(path) || depth >= MAX_DEPTH || !self.processed.insert(style.key(path)) {
            return None;
        }
        let ext = extension(&style.file_name(path));
        if !ext.is_empty() && !TEXT_EXTENSIONS.contains(&ext.as_str()) {
            tracing::debug!(path, "skipping non-text file in @include");
            return None;
        }
        match self.scope.stat_kind(path).await? {
            (FileKind::File, size) if size <= MAX_BYTES => {}
            _ => return None,
        }
        let raw = self.scope.read(path).await?;
        let (content, globs, includes) = parse(&raw);
        if content.trim().is_empty() {
            return None;
        }
        let file = RuleFile {
            path: path.to_owned(),
            kind,
            content,
            globs,
            parent,
        };
        Some((file, includes))
    }

    /// 文件与它 include 的文件。`external` 为 false 时不跟到工作区外。
    pub(crate) async fn file(&mut self, path: String, kind: Kind, external: bool) -> Vec<RuleFile> {
        let (style, cwd, home) = (self.scope.style, &self.scope.cwd, self.scope.home.clone());
        let mut out = Vec::new();
        let mut stack = vec![(path, 0, None)];
        while let Some((path, depth, parent)) = stack.pop() {
            let Some((file, includes)) = self.one(&path, kind, depth, parent).await else {
                continue;
            };
            let base = style.parent(&path).unwrap_or_else(|| path.clone());
            let mut next: Vec<String> = Vec::new();
            for include in includes {
                let resolved = style.resolve(&base, &include, home.as_deref());
                if (external || style.is_within(&resolved, cwd)) && !next.contains(&resolved) {
                    next.push(resolved);
                }
            }
            out.push(file);
            stack.extend(
                next.into_iter()
                    .rev()
                    .map(|include| (include, depth + 1, Some(path.clone()))),
            );
        }
        out
    }

    /// 规则目录里全部 `.md` 文件与它们的 include，条件与否由调用方筛。
    pub(crate) async fn rules(&mut self, dir: String, kind: Kind, external: bool) -> Vec<RuleFile> {
        let style = self.scope.style;
        let mut visited = HashSet::new();
        let mut out = Vec::new();
        let mut stack = vec![(dir, true)];
        while let Some((path, is_dir)) = stack.pop() {
            if !is_dir {
                out.extend(self.file(path, kind, external).await);
                continue;
            }
            if !visited.insert(style.key(&path)) {
                continue;
            }
            let entries = self.scope.list(&path).await;
            let children = entries.into_iter().filter_map(|(name, entry)| match entry {
                FileKind::Directory => Some((style.join(&path, &[&name]), true)),
                FileKind::File if name.ends_with(".md") => {
                    Some((style.join(&path, &[&name]), false))
                }
                FileKind::File | FileKind::Other => None,
            });
            stack.extend(children.rev());
        }
        out
    }
}

/// 只留无条件规则（启动时宣布的那些）。
pub(crate) fn unconditional(files: Vec<RuleFile>) -> impl Iterator<Item = RuleFile> {
    files.into_iter().filter(|file| file.globs.is_none())
}
