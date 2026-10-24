//! Bash 读文件记为已读（docs/specs/rust-cli/tools-files.md §4.2）。
//!
//! 前台命令正常结束后认出单纯读文件的命令（`cat`、`nl`、`bat`、`head`、`tail`、`sed -n`、单文件的 `grep` / `rg`），
//! 从执行环境读一次文件写进 readFileState：内容取文件本身，不取命令输出。已经有记录的文件不覆盖。

use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::env_files::{FileKind, ReadText, TextRead};
use rust_cli_kernel_api::tool::ToolUseContext;

use crate::common::{cwd, resolve_input_stable, stat};
use crate::file_state::{Baseline, FileState, ReadFileState};

/// `head` / `tail` 没给行数时的行数。
const DEFAULT_LINES: u64 = 10;
/// 超过这么大的文件不记。
const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// 记下的范围。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Range {
    /// 全文。
    Whole,
    /// 第 `start` 行到第 `end` 行（1 起，含两端）。
    Lines { start: u64, end: u64 },
    /// 最后 `count` 行。
    Tail { count: u64 },
}

/// 一个认出的读文件命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReadCommand {
    pub(crate) path: String,
    pub(crate) range: Range,
    /// grep / rg：退出码必须是 0，记录的内容不算在上下文里（模型只看到了匹配的行）。
    pub(crate) search: bool,
}

fn whole(path: &str, search: bool) -> ReadCommand {
    ReadCommand {
        path: path.to_owned(),
        range: Range::Whole,
        search,
    }
}

fn digits(text: &str) -> Option<u64> {
    (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

/// `sed -n 'N,Mp' f` / `sed -n 'Np' f`。
fn sed(argv: &[String]) -> Option<ReadCommand> {
    if argv.first().map(String::as_str) != Some("sed") {
        return None;
    }
    let (mut quiet, mut positionals) = (false, Vec::new());
    for arg in &argv[1..] {
        if let Some(long) = arg.strip_prefix("--") {
            if long == "in-place" || long.starts_with("in-place=") || long == "expression" {
                return None;
            }
            quiet |= long == "quiet" || long == "silent";
        } else if arg.starts_with('-') {
            if arg.contains('i') || arg == "-e" {
                return None;
            }
            quiet |= arg.contains('n');
        } else {
            positionals.push(arg.as_str());
        }
    }
    let [script, file] = positionals[..] else {
        return None;
    };
    if !quiet {
        return None;
    }
    let script = script.strip_suffix('p')?;
    let (start, end) = match script.split_once(',') {
        Some((start, end)) => (digits(start)?, digits(end)?),
        None => {
            let line = digits(script)?;
            (line, line)
        }
    };
    Some(ReadCommand {
        path: file.to_owned(),
        range: Range::Lines { start, end },
        search: false,
    })
}

/// `cat`、`nl`、`bat`、`batcat`：一个文件，只有列出的选项。
fn cat(argv: &[String]) -> Option<ReadCommand> {
    let allowed: &[&str] = match argv.first().map(String::as_str)? {
        "cat" => &["-n", "--number"],
        "nl" => &[],
        "bat" | "batcat" => &["-n", "--number", "-p", "--plain"],
        _ => return None,
    };
    let mut file = None;
    for arg in &argv[1..] {
        if arg.starts_with('-') && arg != "-" {
            if !allowed.contains(&arg.as_str()) {
                return None;
            }
            continue;
        }
        if file.replace(arg.as_str()).is_some() {
            return None;
        }
    }
    file.filter(|file| *file != "-")
        .map(|file| whole(file, false))
}

/// `head` / `tail` 的行数与文件：`-n N`、`--lines N`、`--lines=N`、`-nN`、`-N`。
fn lines_and_file(argv: &[String]) -> Option<(u64, &str)> {
    let (mut count, mut file) = (None, None);
    let mut args = argv[1..].iter();
    while let Some(arg) = args.next() {
        if arg == "-n" || arg == "--lines" {
            count = Some(digits(args.next()?)?);
        } else if let Some(value) = arg.strip_prefix("--lines=") {
            count = Some(digits(value)?);
        } else if let Some(value) = arg
            .strip_prefix("-n")
            .filter(|value| digits(value).is_some())
        {
            count = digits(value);
        } else if let Some(value) = arg
            .strip_prefix('-')
            .filter(|value| digits(value).is_some())
        {
            count = digits(value);
        } else if arg.starts_with('-') || file.replace(arg.as_str()).is_some() {
            // 不认的选项、第二个文件：都不认。
            return None;
        }
    }
    let file = file.filter(|file| *file != "-")?;
    match count {
        Some(0) => None,
        count => Some((count.unwrap_or(DEFAULT_LINES), file)),
    }
}

fn head(argv: &[String]) -> Option<ReadCommand> {
    if argv.first().map(String::as_str) != Some("head") {
        return None;
    }
    let (count, file) = lines_and_file(argv)?;
    Some(ReadCommand {
        path: file.to_owned(),
        range: Range::Lines {
            start: 1,
            end: count,
        },
        search: false,
    })
}

fn tail(argv: &[String]) -> Option<ReadCommand> {
    if argv.first().map(String::as_str) != Some("tail") {
        return None;
    }
    let (count, file) = lines_and_file(argv)?;
    Some(ReadCommand {
        path: file.to_owned(),
        range: Range::Tail { count },
        search: false,
    })
}

/// 选项是 `-` 加这些字母的组合。
fn bundle(arg: &str, letters: &str) -> bool {
    arg.strip_prefix('-')
        .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| letters.contains(c)))
}

/// `-A3`、`-B10`、`-C2` 一类。
fn context_bundle(arg: &str) -> bool {
    let mut chars = arg.chars();
    chars.next() == Some('-')
        && matches!(chars.next(), Some('A' | 'B' | 'C'))
        && digits(chars.as_str()).is_some()
}

/// 单文件的 `grep` / `egrep` / `fgrep` / `rg`：一个模式、一个文件，只有列出的选项。
fn grep(argv: &[String]) -> Option<ReadCommand> {
    const GREP_LONG: &[&str] = &[
        "--line-number",
        "--ignore-case",
        "--word-regexp",
        "--line-regexp",
        "--extended-regexp",
        "--fixed-strings",
        "--basic-regexp",
        "--perl-regexp",
        "--with-filename",
        "--no-filename",
        "--color=never",
        "--color=auto",
    ];
    const RG_LONG: &[&str] = &[
        "--ignore-case",
        "--smart-case",
        "--case-sensitive",
        "--word-regexp",
        "--line-regexp",
        "--fixed-strings",
        "--line-number",
        "--no-line-number",
        "--with-filename",
        "--no-filename",
        "--multiline",
        "--pcre2",
    ];
    let rg = match argv.first().map(String::as_str)? {
        "rg" => true,
        "grep" | "egrep" | "fgrep" => false,
        _ => return None,
    };
    let (letters, long) = if rg {
        ("iSswxFnNHUP", RG_LONG)
    } else {
        ("niwxEFGPHh", GREP_LONG)
    };
    let mut positionals = Vec::new();
    let mut args = argv[1..].iter();
    while let Some(arg) = args.next() {
        if !(arg.starts_with('-') && arg != "-") {
            positionals.push(arg.as_str());
            continue;
        }
        let separate = matches!(arg.as_str(), "-A" | "-B" | "-C")
            || (rg
                && matches!(
                    arg.as_str(),
                    "--after-context" | "--before-context" | "--context"
                ));
        if separate {
            digits(args.next()?)?;
            continue;
        }
        let assigned = ["--after-context=", "--before-context=", "--context="]
            .iter()
            .any(|prefix| arg.strip_prefix(prefix).and_then(digits).is_some());
        if !(context_bundle(arg)
            || assigned
            || bundle(arg, letters)
            || long.contains(&arg.as_str()))
        {
            return None;
        }
    }
    let [_, file] = positionals[..] else {
        return None;
    };
    if file == "-" || file.contains(['*', '?', '[', '{']) {
        return None;
    }
    Some(whole(file, true))
}

/// `echo`、`printf`、`true`、`:` 开头的段（多段命令里可以夹着它们）。
fn harmless(segment: &str) -> bool {
    let text = segment.trim_start();
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    for name in ["echo", "printf", "true"] {
        if let Some(rest) = text.strip_prefix(name)
            && !rest.starts_with(word)
        {
            return true;
        }
    }
    // `:` 后面要紧跟一个单词字符才算词边界。
    text.strip_prefix(':')
        .is_some_and(|rest| rest.starts_with(word))
}

/// 整条命令里认出的读文件命令；有一段认不出（又不是 `echo` 一类）时整条都不算。
pub(crate) fn recognize(command: &str) -> Vec<ReadCommand> {
    if command.contains(['|', '<', '>']) {
        return Vec::new();
    }
    let analysis = rust_cli_policy::analyze(command);
    if analysis.has_parse_errors || analysis.has_unsupported_syntax || analysis.commands.is_empty()
    {
        return Vec::new();
    }
    let single = analysis.commands.len() == 1;
    let mut found = Vec::new();
    for invocation in &analysis.commands {
        let argv = &invocation.argv;
        let recognized = (!invocation.has_dynamic_words)
            .then(|| {
                sed(argv)
                    .or_else(|| cat(argv))
                    .or_else(|| head(argv))
                    .or_else(|| tail(argv))
                    .or_else(|| single.then(|| grep(argv)).flatten())
            })
            .flatten();
        match recognized {
            Some(read) => found.push(read),
            None if !single && !harmless(&invocation.command_text) => return Vec::new(),
            None => {}
        }
    }
    found
}

/// 记录的内容与范围（`offset`、`limit`）；范围在文件之外时为 None。
pub(crate) fn shape(content: &str, range: &Range) -> Option<(String, Option<u64>, Option<u64>)> {
    match *range {
        Range::Whole => Some((content.to_owned(), None, None)),
        Range::Lines { start, end } => {
            let lines: Vec<&str> = content.split('\n').collect();
            let start = start.max(1);
            let end = end.max(start);
            let first = usize::try_from(start).ok()?;
            if first > lines.len() {
                return None;
            }
            let last = usize::try_from(end).unwrap_or(usize::MAX).min(lines.len());
            let text = lines[first - 1..last].join("\n");
            Some((text, Some(start), Some(end - start + 1)))
        }
        Range::Tail { count } => {
            let mut lines: Vec<&str> = content.split('\n').collect();
            if lines.last() == Some(&"") {
                lines.pop();
            }
            if lines.is_empty() {
                return None;
            }
            let shown = usize::try_from(count)
                .unwrap_or(usize::MAX)
                .min(lines.len());
            let first = lines.len() - shown + 1;
            let text = lines[first - 1..].join("\n");
            Some((text, Some(first as u64), Some(shown as u64)))
        }
    }
}

/// 命令正常结束之后：把认出的文件记进这个会话的 readFileState。`truncated` 是输出太长、给的是预览。
pub(crate) async fn record(
    state: &ReadFileState,
    context: &ToolUseContext,
    command: &str,
    (exit_code, truncated): (i32, bool),
) {
    let reads: Vec<ReadCommand> = recognize(command)
        .into_iter()
        .filter(|read| !read.search || exit_code == 0)
        .collect();
    if reads.is_empty() {
        return;
    }
    let base = cwd(context).await;
    for read in reads {
        let Ok(Some(path)) = resolve_input_stable(context, &base, &read.path).await else {
            continue;
        };
        if state
            .get_at(&context.session, context.machine.name(), &path)
            .is_some()
        {
            continue;
        }
        match stat(context, &path).await {
            Ok(Some(found)) if found.kind == FileKind::File && found.size <= MAX_FILE_BYTES => {}
            _ => continue,
        }
        if context.cancel.is_cancelled() {
            return;
        }
        let request = ReadText {
            path: EnvPath::new(path.clone()),
            start_line: 0,
            max_lines: None,
            max_bytes: Some(MAX_FILE_BYTES),
        };
        let Ok(TextRead::Text(slice)) = context
            .machine
            .files()
            .read_text(&context.workspace, request)
            .await
        else {
            continue;
        };
        let Some((content, offset, limit)) = shape(&slice.content, &read.range) else {
            continue;
        };
        state.set_at(
            &context.session,
            context.machine.name(),
            &path,
            FileState {
                content,
                baseline: Baseline::Version(slice.version),
                offset,
                limit,
                partial: false,
                not_in_context: read.search || truncated,
            },
        );
    }
}

#[cfg(test)]
#[path = "bash_reads_tests.rs"]
mod tests;
