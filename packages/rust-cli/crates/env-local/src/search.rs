//! 文件名搜索与内容搜索（docs/specs/rust-cli/tools-files.md §3）。
//!
//! 选型：用 ripgrep 自己的库（`ignore` 遍历与忽略规则、`--glob` / `--type` 语义；`grep-regex` /
//! `grep-searcher` 匹配、行号、上下文、二进制检测、BOM 转码），不调用 rg 二进制：得到 rg 的
//! 同一套语义，又不依赖用户机器上装没装 rg。
//!
//! 与 rg 的有意差别：rg 多线程遍历，没有 `--sort` 时文件顺序取决于完成先后；这里单线程、按路径
//! 排序遍历，结果确定。Glob 的 `--sort=modified` 在修改时间相同时 rg 保持遍历顺序，这里再按路径排。

use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use grep_matcher::Matcher;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkMatch};
use ignore::overrides::OverrideBuilder;
use ignore::types::TypesBuilder;
use ignore::{DirEntry, WalkBuilder};
use rust_cli_kernel_api::env::{EnvError, EnvPath};
use rust_cli_kernel_api::env_files::{
    FileList, GlobFiles, GlobMatches, GrepCount, GrepFileHit, GrepFileLines, GrepLine,
    GrepLineKind, GrepMode, GrepOutcome, GrepSearch, GrepSubmatch, ListFiles, VCS_DIRS,
};
use tokio_util::sync::CancellationToken;

use crate::meta::{display, mtime_ms};

fn walker(root: &Path, hidden: bool, respect_ignore: bool) -> WalkBuilder {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(!hidden)
        .ignore(respect_ignore)
        .git_ignore(respect_ignore)
        .git_global(respect_ignore)
        .git_exclude(respect_ignore)
        .parents(respect_ignore)
        .follow_links(false)
        .sort_by_file_path(Ord::cmp);
    if respect_ignore {
        builder.add_custom_ignore_filename(".rgignore");
    }
    builder
}

/// 遍历的条目，每个条目先看一次取消（R5-13）：取消后不再往下走（结果已经没有人等）。
fn entries<'a>(
    builder: &WalkBuilder,
    stop: &'a CancellationToken,
) -> impl Iterator<Item = DirEntry> + 'a {
    builder
        .build()
        .take_while(move |_| !stop.is_cancelled())
        .flatten()
}

fn is_file(entry: &DirEntry) -> bool {
    entry.file_type().is_some_and(|kind| kind.is_file())
}

fn modified(entry: &DirEntry) -> Option<SystemTime> {
    entry
        .metadata()
        .ok()
        .and_then(|metadata| metadata.modified().ok())
}

pub(crate) fn glob(request: &GlobFiles, stop: &CancellationToken) -> Result<GlobMatches, EnvError> {
    let root = PathBuf::from(request.root.as_str());
    let mut overrides = OverrideBuilder::new(&root);
    let built = overrides
        .add(&request.pattern)
        .and_then(|builder| builder.build())
        .map_err(|error| EnvError::Io {
            message: format!("rg: {error}"),
        })?;
    let mut builder = walker(&root, request.hidden, request.respect_ignore);
    builder.overrides(built);
    let mut found: Vec<(Option<SystemTime>, PathBuf)> = entries(&builder, stop)
        .filter(is_file)
        .map(|entry| (modified(&entry), entry.into_path()))
        .collect();
    // rg --sort=modified：拿不到时间的排在最后。
    found.sort_by(|left, right| match (left.0, right.0) {
        (Some(a), Some(b)) => a.cmp(&b).then_with(|| left.1.cmp(&right.1)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => left.1.cmp(&right.1),
    });
    let total = found.len();
    let files = found
        .into_iter()
        .take(request.limit)
        .map(|(_, path)| EnvPath::new(display(&path)))
        .collect();
    Ok(GlobMatches { files, total })
}

/// 工作区文件清单（`rg --files --hidden` 加忽略规则，跳过版本库目录，不跟随符号链接）：相对 `cwd`，按路径排序。
/// 跳过的版本库目录见 tui-input.md §1.2，表只定义在 kernel-api `VCS_DIRS`
/// （F58：以前这里与 tools 的 Grep 各写一份，同值同序）。
pub(crate) fn list_files(
    request: &ListFiles,
    stop: &CancellationToken,
) -> Result<FileList, EnvError> {
    let root = PathBuf::from(request.cwd.as_str());
    let mut builder = walker(&root, true, true);
    builder.filter_entry(|entry| {
        let vcs = entry.file_type().is_some_and(|kind| kind.is_dir())
            && entry
                .file_name()
                .to_str()
                .is_some_and(|name| VCS_DIRS.contains(&name));
        !vcs
    });
    let files = entries(&builder, stop)
        .filter(is_file)
        .filter_map(|entry| {
            entry
                .path()
                .strip_prefix(&root)
                .ok()
                .map(|path| path.display().to_string())
        })
        .collect();
    Ok(FileList { files })
}

/// rg 的匹配器配置（`hiargs.rs` 的 matcher_rust）。
fn matcher(request: &GrepSearch) -> Result<RegexMatcher, String> {
    let mut builder = RegexMatcherBuilder::new();
    builder
        .multi_line(true)
        .unicode(true)
        .octal(false)
        .case_insensitive(request.case_insensitive);
    if request.multiline {
        builder.dot_matches_new_line(true);
    } else {
        builder
            .line_terminator(Some(b'\n'))
            .dot_matches_new_line(false);
    }
    builder
        .build_many(&[request.pattern.as_str()])
        .map_err(|error| error.to_string())
}

fn search_walker(request: &GrepSearch, root: &Path) -> Result<WalkBuilder, String> {
    let mut builder = walker(root, request.hidden, true);
    if !request.globs.is_empty() {
        let mut overrides = OverrideBuilder::new(root);
        for glob in &request.globs {
            overrides.add(glob).map_err(|error| error.to_string())?;
        }
        builder.overrides(overrides.build().map_err(|error| error.to_string())?);
    }
    if let Some(name) = &request.file_type {
        let mut types = TypesBuilder::new();
        types.add_defaults();
        types.select(name);
        builder.types(types.build().map_err(|error| error.to_string())?);
    }
    Ok(builder)
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// 收集一个文件的匹配；`first_only` 时第一处匹配后停止（rg -l）。
struct Collect<'a> {
    matcher: &'a RegexMatcher,
    keep_lines: bool,
    first_only: bool,
    matches: u64,
    lines: Vec<GrepLine>,
    binary_offset: Option<u64>,
}

impl Sink for Collect<'_> {
    type Error = io::Error;

    fn matched(&mut self, _searcher: &Searcher, found: &SinkMatch<'_>) -> io::Result<bool> {
        self.matches += 1;
        if self.keep_lines {
            let bytes = found.bytes();
            let mut submatches = Vec::new();
            self.matcher
                .find_iter(bytes, |hit| {
                    submatches.push(GrepSubmatch {
                        start: hit.start() as u64,
                        text: lossy(&bytes[hit]),
                    });
                    true
                })
                .map_err(io::Error::other)?;
            self.lines.push(GrepLine {
                kind: GrepLineKind::Match,
                line_number: found.line_number().unwrap_or(0),
                text: lossy(bytes),
                submatches,
            });
        }
        Ok(!self.first_only)
    }

    fn context(&mut self, _searcher: &Searcher, context: &SinkContext<'_>) -> io::Result<bool> {
        if self.keep_lines {
            self.lines.push(GrepLine {
                kind: GrepLineKind::Context,
                line_number: context.line_number().unwrap_or(0),
                text: lossy(context.bytes()),
                submatches: Vec::new(),
            });
        }
        Ok(true)
    }

    fn binary_data(&mut self, _searcher: &Searcher, offset: u64) -> io::Result<bool> {
        self.binary_offset = Some(offset);
        Ok(true)
    }
}

pub(crate) fn grep(
    request: &GrepSearch,
    stop: &CancellationToken,
) -> Result<GrepOutcome, EnvError> {
    let root = PathBuf::from(request.root.as_str());
    let matcher = match matcher(request) {
        Ok(matcher) => matcher,
        Err(message) => return Ok(GrepOutcome::Rejected { message }),
    };
    let builder = match search_walker(request, &root) {
        Ok(builder) => builder,
        Err(message) => return Ok(GrepOutcome::Rejected { message }),
    };
    let (before, after) = match request.mode {
        GrepMode::Content { before, after } => (before, after),
        GrepMode::FilesWithMatches | GrepMode::Count => (0, 0),
    };
    let mut files = Vec::new();
    let mut counts = Vec::new();
    let mut contents = Vec::new();
    for entry in entries(&builder, stop).filter(is_file) {
        // rg：遍历到的文件遇到 NUL 就停（quit），命令行上直接给的文件按 convert 处理。
        let detection = if entry.depth() == 0 {
            BinaryDetection::convert(b'\x00')
        } else {
            BinaryDetection::quit(b'\x00')
        };
        let mut searcher = SearcherBuilder::new()
            .line_number(true)
            .multi_line(request.multiline)
            .before_context(usize::try_from(before).unwrap_or(usize::MAX))
            .after_context(usize::try_from(after).unwrap_or(usize::MAX))
            .binary_detection(detection)
            .build();
        let mut sink = Collect {
            matcher: &matcher,
            keep_lines: matches!(request.mode, GrepMode::Content { .. }),
            first_only: matches!(request.mode, GrepMode::FilesWithMatches),
            matches: 0,
            lines: Vec::new(),
            binary_offset: None,
        };
        // 读不了的文件跳过（rg 打印到 stderr 后继续）。
        if searcher
            .search_path(&matcher, entry.path(), &mut sink)
            .is_err()
            || sink.matches == 0
        {
            continue;
        }
        let path = EnvPath::new(display(entry.path()));
        match request.mode {
            GrepMode::FilesWithMatches => files.push(GrepFileHit {
                path,
                mtime_ms: entry.metadata().map_or(0, |metadata| mtime_ms(&metadata)),
            }),
            GrepMode::Count => counts.push(GrepCount {
                path,
                count: sink.matches,
            }),
            GrepMode::Content { .. } => contents.push(GrepFileLines {
                path,
                lines: sink.lines,
                binary_offset: sink.binary_offset,
            }),
        }
    }
    Ok(match request.mode {
        GrepMode::FilesWithMatches => GrepOutcome::Files { files },
        GrepMode::Count => GrepOutcome::Counts { counts },
        GrepMode::Content { .. } => GrepOutcome::Content { files: contents },
    })
}

#[cfg(test)]
#[path = "search_tests.rs"]
mod tests;
