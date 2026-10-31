//! `zcode --help`（docs/specs/rust-cli/cli-args.md §3）：帮助的固定文字，只列本工程
//! 支持的选项与命令。排版规则（
//! 项缩进 2、名字一栏最宽 36、名字与说明之间 2 个空格、说明一栏不足 30 列或名字太长时说明另起一行缩进 6；
//! 按单词折行），宽度是终端的列数，不是终端时 80（commander `helpWidth`）。

use crate::commander::locale_compare;
use crate::consts;

/// 帮助里的一项：名字一栏与说明。
#[derive(Debug, Clone, Copy)]
pub(crate) struct HelpItem {
    pub(crate) term: &'static str,
    pub(crate) description: &'static str,
}

const fn item(term: &'static str, description: &'static str) -> HelpItem {
    HelpItem { term, description }
}

const USAGE: &str = "zcode [options] [command] [prompt]";
const DESCRIPTION: &str =
    "ZCode - starts an interactive session by default, use -p/--print for non-interactive output";

const ARGUMENTS: &[HelpItem] = &[item("prompt", "Your prompt")];

/// 列出的选项（说明的文字固定；改过的说明登记在 cli-args.md §3.2）。输出时按 `option_key` 排序。
pub(crate) const OPTIONS: &[HelpItem] = &[
    item(
        "--add-dir <directories...>",
        "Additional directories to allow tool access to",
    ),
    item(
        "--allowedTools, --allowed-tools <tools...>",
        "Comma or space-separated list of tool names to allow (e.g. \"Bash(git *) Edit\")",
    ),
    item(
        "--append-system-prompt <prompt>",
        "Append a system prompt to the default system prompt",
    ),
    item(
        "-c, --continue",
        "Continue the most recent conversation in the current directory",
    ),
    item(
        "--disallowedTools, --disallowed-tools <tools...>",
        "Comma or space-separated list of tool names to deny (e.g. \"Bash(git *) Edit\")",
    ),
    // 工作线 F69（cli-args.md §1.4）：档位的顺序同 `effort::LEVELS`。
    item(
        "--effort <level>",
        "Effort level for the current session (low, medium, high, xhigh, max)",
    ),
    item(
        "--fork-session",
        "When resuming, create a new session ID instead of reusing the original (use with --resume or --continue)",
    ),
    item("-h, --help", "Display help for command"),
    // 格式参数（修复线 F39，headless.md §1）。
    item(
        "--include-partial-messages",
        "Include partial message chunks as they arrive (only works with --print and --output-format=stream-json)",
    ),
    item(
        "--input-format <format>",
        "Input format (only works with --print): \"text\" (default), or \"stream-json\" (realtime streaming input) (choices: \"text\", \"stream-json\")",
    ),
    item(
        "--mcp-config <configs...>",
        "Load MCP servers from JSON files or strings (space-separated)",
    ),
    item("--model <model>", "Model for the current session."),
    item(
        "--output-format <format>",
        "Output format (only works with --print): \"text\" (default), \"json\" (single result), or \"stream-json\" (realtime streaming) (choices: \"text\", \"json\", \"stream-json\")",
    ),
    // 工作线 F63（cli-args.md §3.2）：原有的 choices 之后多一个本工程的 `yolo`（与报错的顺序相同）；
    // `--dangerously-skip-permissions` 删掉了，不列。
    item(
        "--permission-mode <mode>",
        "Permission mode to use for the session (choices: \"acceptEdits\", \"auto\", \"bypassPermissions\", \"manual\", \"dontAsk\", \"plan\", \"yolo\")",
    ),
    item(
        "--plugin-dir <path>",
        "Load a plugin from a directory for this session only (repeatable: --plugin-dir A --plugin-dir B) (default: [])",
    ),
    item(
        "-p, --print",
        "Print response and exit (useful for pipes). Note: The workspace trust dialog is skipped when ZCode is run in non-interactive mode (via -p). Only use this in directories you trust.",
    ),
    item(
        "-r, --resume [value]",
        "Resume a conversation by session ID, or open interactive picker",
    ),
    item(
        "--session-id <uuid>",
        "Use a specific session ID for the conversation (must be a valid UUID)",
    ),
    item(
        "--settings <file-or-json>",
        "Path to a settings JSON file or a JSON string to load additional settings from",
    ),
    item(
        "--strict-mcp-config",
        "Only use MCP servers from --mcp-config, ignoring all other MCP configurations",
    ),
    item(
        "--system-prompt <prompt>",
        "System prompt to use for the session",
    ),
    item("--verbose", "Override verbose mode setting from config"),
    item("-v, --version", "Output the version number"),
];

/// 本工程的命令（cli-args.md §3.2）：`hooks`、`login`、`logout` 是 Node 版 zcode 的；`update|upgrade` 的名字固定，
/// 本工程照 Codex 不先检查，说明用 Codex 的（工作线 F48，update.md §9）。
pub(crate) const COMMANDS: &[HelpItem] = &[
    item(
        "hooks",
        "Trust project hooks (zcode hooks trust --help for options)",
    ),
    item(
        "login [target]",
        "Log in to a Coding Plan account (zai, bigmodel, or <family>-coding-plan-api-key to read an API key from stdin)",
    ),
    item("logout", "Log out from Coding Plan accounts"),
    item("update|upgrade", "Update ZCode to the latest version"),
];

/// commander 的项缩进、名字一栏的上限、名字与说明的间隔、说明一栏的最小宽度、另起一行时的缩进增量。
const INDENT: usize = 2;
const MAX_TERM: usize = 36;
const GAP: usize = 2;
const MIN_DESCRIPTION: usize = 30;
const STACKED: usize = 4;
/// 不是终端时的宽度（commander `helpWidth || 80`）。
pub(crate) const DEFAULT_WIDTH: usize = 80;

fn width_of(text: &str) -> usize {
    text.chars().count()
}

/// 按单词折行：每行按 `\s*\S+` 切成带前导空白的词；放不下时另起一行，去掉这个词的前导空白。
fn wrap(text: &str, width: usize) -> String {
    let width = width.max(1);
    let mut lines = Vec::new();
    for line in text.split('\n') {
        let mut words = Vec::new();
        let mut start = 0;
        let mut seen_text = false;
        for (index, c) in line.char_indices() {
            if c.is_whitespace() {
                if seen_text {
                    words.push(&line[start..index]);
                    start = index;
                    seen_text = false;
                }
            } else {
                seen_text = true;
            }
        }
        if seen_text {
            words.push(&line[start..]);
        }
        let Some((first, rest)) = words.split_first() else {
            lines.push(String::new());
            continue;
        };
        let mut current = (*first).to_owned();
        let mut used = width_of(first);
        for word in rest {
            let size = width_of(word);
            if used + size <= width {
                current.push_str(word);
                used += size;
            } else {
                lines.push(std::mem::take(&mut current));
                let trimmed = word.trim_start();
                current.push_str(trimmed);
                used = width_of(trimmed);
            }
        }
        lines.push(current);
    }
    lines.join("\n")
}

/// 一项。
fn format_item(item: &HelpItem, pad: usize, width: usize) -> String {
    let indent = " ".repeat(INDENT);
    let term_width = width_of(item.term);
    let available = width.saturating_sub(INDENT + pad + GAP);
    if term_width <= pad && available >= MIN_DESCRIPTION {
        let spacing = " ".repeat(pad - term_width + GAP);
        let continued = format!("\n{}", " ".repeat(INDENT + pad + GAP));
        let text = wrap(item.description, available).replace('\n', &continued);
        return format!("{indent}{}{spacing}{text}", item.term);
    }
    let stacked = " ".repeat(INDENT + STACKED);
    let text = wrap(item.description, width.saturating_sub(INDENT + STACKED))
        .replace('\n', &format!("\n{stacked}"));
    format!("{indent}{}\n{stacked}{text}", item.term)
}

/// commander 12 的 `splitOptionFlags`：flags 按空格、`|`、逗号切开；第二段不是 `<…>` / `[…]` 时第一段是短名字，
/// 下一段是长名字（所以 `--allowedTools, --allowed-tools` 的长名字是 `--allowed-tools`）。
fn split_flags(term: &str) -> (Option<&str>, Option<&str>) {
    let mut parts = term
        .split([' ', '|', ','])
        .filter(|part| !part.is_empty())
        .peekable();
    let first = parts.next();
    let second = parts.peek().copied();
    let (short, long) = match second {
        Some(next) if !next.starts_with(['[', '<']) => (first, parts.next()),
        _ => (None, first),
    };
    match (short, long) {
        (None, Some(long)) if long.len() == consts::SHORT_OPTION_LEN && !long.starts_with("--") => {
            (Some(long), None)
        }
        other => other,
    }
}

/// 选项按 commander 的 `compareOptions` 排：长名字（没有时短名字）去掉前导 `-` 后按 `localeCompare`。
fn option_key(term: &str) -> &str {
    match split_flags(term) {
        (_, Some(long)) => long.trim_start_matches("--"),
        (Some(short), None) => short.trim_start_matches('-'),
        (None, None) => term,
    }
}

/// 帮助全文（以换行结尾）。`width` 是终端列数，不是终端时用 [`DEFAULT_WIDTH`]。
pub(crate) fn render(width: usize) -> String {
    let width = if width == 0 { DEFAULT_WIDTH } else { width };
    let mut options: Vec<&HelpItem> = OPTIONS.iter().collect();
    options.sort_by(|a, b| locale_compare(option_key(a.term), option_key(b.term)));
    let mut commands: Vec<&HelpItem> = COMMANDS.iter().collect();
    commands.sort_by(|a, b| locale_compare(a.term, b.term));
    let pad = ARGUMENTS
        .iter()
        .chain(options.iter().copied())
        .chain(commands.iter().copied())
        .map(|item| width_of(item.term))
        .max()
        .unwrap_or(0)
        .min(MAX_TERM);
    let mut out = vec![format!("Usage: {USAGE}"), String::new()];
    out.push(wrap(DESCRIPTION, width));
    out.push(String::new());
    let mut section = |title: &str, items: &[&HelpItem]| {
        out.push(title.to_owned());
        out.extend(items.iter().map(|item| format_item(item, pad, width)));
        out.push(String::new());
    };
    section("Arguments:", &ARGUMENTS.iter().collect::<Vec<_>>());
    section("Options:", &options);
    section("Commands:", &commands);
    out.join("\n")
}

/// 帮助里列出的长选项（未知选项的建议只在这些里找，commander 的 `visibleOptions` 的 `long`）。
pub(crate) fn visible_longs() -> Vec<&'static str> {
    OPTIONS
        .iter()
        .filter_map(|item| split_flags(item.term).1)
        .collect()
}

#[cfg(test)]
#[path = "help_tests.rs"]
mod tests;
