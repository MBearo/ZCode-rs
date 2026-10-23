//! 命令行参数（docs/specs/rust-cli/cli-args.md）：选项的写法与含义（commander 的切分规则见
//! `commander.rs`），续写见 session-lifecycle.md §1，权限参数见 shell-permissions.md §5。
//!
//! 这里只做与 IO 无关的解析与校验（commander 里抛的错误：缺值、取值不合法、未知选项）；要读文件或看目录
//! 的校验（`--system-prompt-file`、`--settings`、`--add-dir`）在入口读完环境之后做（`flags.rs`）。

use rust_cli_headless::SessionTarget;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::permission::{
    PermissionMode, PermissionRules, RuleBehavior, RuleSource, SourcedRule,
};
use rust_cli_policy::rules::parse_rule;

use crate::commander::{Arity, OptionSpec, split, suggestion};
use crate::mcp::McpFlags;
use crate::print_format::{self, FormatFlags};
use crate::{effort, help};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Version,
    /// `-h` / `--help`（cli-args.md §3）。
    Help,
    /// `-p` / `--print`；prompt 是位置参数，没有时从标准输入读（cli-args.md §2.3）。
    Print,
    Tui,
}

/// system prompt 的一段（cli-args.md §4）：命令行给的文字与要读的文件；两个都给时把文件内容（去掉一个结尾换行）
/// 与文字用空行连接。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PromptFlags {
    pub(crate) inline: Option<String>,
    pub(crate) file: Option<String>,
}

/// 命令行（cli-args.md §1 的选项表）。
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Invocation {
    pub(crate) mode: Mode,
    /// 位置参数 `[prompt]`（commander 只取第一个，多余的忽略）。
    pub(crate) prompt: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) target: SessionTarget,
    /// `--permission-mode`（本工程的 `yolo` 已在解析时换成 bypassPermissions，cli-args.md §5.3）。
    pub(crate) permission_mode: Option<PermissionMode>,
    /// `--allowedTools` / `--disallowedTools` 的规则文本（命令行来源，shell-permissions.md §5）。
    pub(crate) allowed: Vec<String>,
    pub(crate) disallowed: Vec<String>,
    /// `--max-turns`。
    pub(crate) max_turns: Option<u32>,
    /// `--effort` 的档位（cli-args.md §1.4）；没给或不认识时为空。
    pub(crate) effort: Option<&'static str>,
    /// 解析时的警告（`--effort` 的值不认识），入口写到 stderr，不中断运行。
    pub(crate) warnings: Vec<String>,
    /// `--mcp-config` / `--strict-mcp-config`（docs/specs/rust-cli/ext.md §3.1）。
    pub(crate) mcp: McpFlags,
    /// `--plugin-dir`（ext.md §4）。
    pub(crate) plugin_dirs: Vec<String>,
    /// `--resume` 不带 ID：TUI 启动后打开会话选择器（session-lifecycle.md §6.2）。
    pub(crate) resume_pick: bool,
    /// `--fork-session`：与 `-c` / `-r` 一起用，打开后先 fork；单独使用时为 false（忽略它）。
    pub(crate) fork_session: bool,
    /// `--system-prompt` / `--system-prompt-file`（cli-args.md §4）。
    pub(crate) system_prompt: PromptFlags,
    /// `--append-system-prompt` / `--append-system-prompt-file`。
    pub(crate) append_system_prompt: PromptFlags,
    /// `--add-dir`（cli-args.md §6）：原样的参数，入口按 cwd 解析并检查。
    pub(crate) add_dirs: Vec<String>,
    /// `--session-id`（cli-args.md §7）：已校验是 UUID。
    pub(crate) session_id: Option<String>,
    /// `--settings`：文件路径或 JSON 文本（cli-args.md §8）。
    pub(crate) settings: Option<String>,
    /// `--output-format`、`--input-format`、`--verbose`、`--include-partial-messages`（headless.md §1，修复线 F39）。
    pub(crate) format: FormatFlags,
}

impl Default for Invocation {
    fn default() -> Self {
        Self {
            mode: Mode::Tui,
            prompt: None,
            model: None,
            target: SessionTarget::New,
            permission_mode: None,
            allowed: Vec::new(),
            disallowed: Vec::new(),
            max_turns: None,
            effort: None,
            warnings: Vec::new(),
            mcp: McpFlags::default(),
            plugin_dirs: Vec::new(),
            resume_pick: false,
            fork_session: false,
            system_prompt: PromptFlags::default(),
            append_system_prompt: PromptFlags::default(),
            add_dirs: Vec::new(),
            session_id: None,
            settings: None,
            format: FormatFlags::default(),
        }
    }
}

/// 选项的身份（commander 的 `option:<name>` 事件）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    AddDir,
    Allowed,
    AppendSystemPrompt,
    AppendSystemPromptFile,
    Continue,
    Disallowed,
    Effort,
    ForkSession,
    IncludePartialMessages,
    InputFormat,
    MaxTurns,
    McpConfig,
    Model,
    OutputFormat,
    PermissionMode,
    PluginDir,
    Print,
    Resume,
    SessionId,
    Settings,
    StrictMcpConfig,
    SystemPrompt,
    SystemPromptFile,
    Verbose,
    Version,
}

const fn spec(
    key: Key,
    flags: &'static str,
    short: Option<&'static str>,
    longs: &'static [&'static str],
    arity: Arity,
) -> OptionSpec<Key> {
    OptionSpec {
        key,
        flags,
        short,
        longs,
        arity,
    }
}

/// 认的选项（不在帮助里列出的也在这里，例如 `--max-turns`、`--system-prompt-file`）。
/// `--dangerously-skip-permissions` 不在这里（工作线 F63，用户决定 2026-10-22：`--permission-mode yolo` 覆盖它），
/// 传它时是未知选项（cli-args.md §5.3）。
const SPECS: &[OptionSpec<Key>] = &[
    spec(
        Key::AddDir,
        "--add-dir <directories...>",
        None,
        &["--add-dir"],
        Arity::Variadic,
    ),
    spec(
        Key::Allowed,
        "--allowedTools, --allowed-tools <tools...>",
        None,
        &["--allowedTools", "--allowed-tools"],
        Arity::Variadic,
    ),
    spec(
        Key::AppendSystemPrompt,
        "--append-system-prompt <prompt>",
        None,
        &["--append-system-prompt"],
        Arity::Required,
    ),
    spec(
        Key::AppendSystemPromptFile,
        "--append-system-prompt-file <file>",
        None,
        &["--append-system-prompt-file"],
        Arity::Required,
    ),
    spec(
        Key::Continue,
        "-c, --continue",
        Some("-c"),
        &["--continue"],
        Arity::Flag,
    ),
    spec(
        Key::Disallowed,
        "--disallowedTools, --disallowed-tools <tools...>",
        None,
        &["--disallowedTools", "--disallowed-tools"],
        Arity::Variadic,
    ),
    spec(
        Key::Effort,
        "--effort <level>",
        None,
        &["--effort"],
        Arity::Required,
    ),
    spec(
        Key::ForkSession,
        "--fork-session",
        None,
        &["--fork-session"],
        Arity::Flag,
    ),
    spec(
        Key::IncludePartialMessages,
        "--include-partial-messages",
        None,
        &["--include-partial-messages"],
        Arity::Flag,
    ),
    spec(
        Key::InputFormat,
        "--input-format <format>",
        None,
        &["--input-format"],
        Arity::Required,
    ),
    spec(
        Key::MaxTurns,
        "--max-turns <turns>",
        None,
        &["--max-turns"],
        Arity::Required,
    ),
    spec(
        Key::McpConfig,
        "--mcp-config <configs...>",
        None,
        &["--mcp-config"],
        Arity::Variadic,
    ),
    spec(
        Key::Model,
        "--model <model>",
        None,
        &["--model"],
        Arity::Required,
    ),
    spec(
        Key::OutputFormat,
        "--output-format <format>",
        None,
        &["--output-format"],
        Arity::Required,
    ),
    spec(
        Key::PermissionMode,
        "--permission-mode <mode>",
        None,
        &["--permission-mode"],
        Arity::Required,
    ),
    spec(
        Key::PluginDir,
        "--plugin-dir <path>",
        None,
        &["--plugin-dir"],
        Arity::Required,
    ),
    spec(
        Key::Print,
        "-p, --print",
        Some("-p"),
        &["--print"],
        Arity::Flag,
    ),
    spec(
        Key::Resume,
        "-r, --resume [value]",
        Some("-r"),
        &["--resume"],
        Arity::Optional,
    ),
    spec(
        Key::SessionId,
        "--session-id <uuid>",
        None,
        &["--session-id"],
        Arity::Required,
    ),
    spec(
        Key::Settings,
        "--settings <file-or-json>",
        None,
        &["--settings"],
        Arity::Required,
    ),
    spec(
        Key::StrictMcpConfig,
        "--strict-mcp-config",
        None,
        &["--strict-mcp-config"],
        Arity::Flag,
    ),
    spec(
        Key::SystemPrompt,
        "--system-prompt <prompt>",
        None,
        &["--system-prompt"],
        Arity::Required,
    ),
    spec(
        Key::SystemPromptFile,
        "--system-prompt-file <file>",
        None,
        &["--system-prompt-file"],
        Arity::Required,
    ),
    spec(Key::Verbose, "--verbose", None, &["--verbose"], Arity::Flag),
    spec(
        Key::Version,
        "-v, --version",
        Some("-v"),
        &["--version"],
        Arity::Flag,
    ),
];

/// 解析的结果：照常运行，或者已经知道要输出什么（版本）。
enum Stop {
    Version,
    Error(String),
}

/// 命令行给的规则（来源 `CliArg`）与 `--add-dir` 的目录（shell-permissions.md §5、cli-args.md §6）。
pub(crate) fn cli_rules(
    allowed: &[String],
    disallowed: &[String],
    add_dirs: Vec<rust_cli_kernel_api::env::EnvPath>,
) -> PermissionRules {
    let sourced = |behavior: RuleBehavior| {
        move |text: &String| SourcedRule {
            source: RuleSource::CliArg,
            behavior,
            rule: parse_rule(text),
        }
    };
    PermissionRules {
        version: 1,
        rules: allowed
            .iter()
            .map(sourced(RuleBehavior::Allow))
            .chain(disallowed.iter().map(sourced(RuleBehavior::Deny)))
            .collect(),
        additional_directories: add_dirs,
        ..PermissionRules::default()
    }
}

/// 规则里点名了 Glob 或 Grep（`Glob`、`Grep(…)`）。
pub(crate) fn names_search_tools(allowed: &[String]) -> bool {
    allowed
        .iter()
        .any(|rule| matches!(parse_rule(rule).tool.as_str(), "Glob" | "Grep"))
}

/// `--permission-mode` 的取值（cli-args.md §5.1：帮助列出的六个，另认旧名 `default`；同 agent 的
/// frontmatter，kernel-api `from_cli_choice`），加上本工程的 `yolo`（工作线 F63，cli-args.md §5.3：TUI 与 Node 里
/// bypass 模式的名字，等于 bypassPermissions；对外的字段仍写 `bypassPermissions`）。`yolo` 只给命令行，frontmatter 照旧。
fn permission_mode(value: &str) -> Option<PermissionMode> {
    match value {
        "yolo" => Some(PermissionMode::BypassPermissions),
        _ => PermissionMode::from_cli_choice(value),
    }
}

/// 帮助里 `--permission-mode` 的 choices（报错文字用它），本工程的 `yolo` 接在最后（工作线 F63）。
const MODE_CHOICES: &str = "acceptEdits, auto, bypassPermissions, manual, dontAsk, plan, yolo";

/// 工具规则列表：逗号或空白分隔，括号里的不算（`Bash(npm run test, lint)` 是一条）。
fn split_tools(value: &str) -> Vec<String> {
    let mut tools = Vec::new();
    let mut current = String::new();
    let mut depth = 0usize;
    for character in value.chars() {
        match character {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' | ' ' | '\t' | '\n' if depth == 0 => {
                if !current.trim().is_empty() {
                    tools.push(current.trim().to_owned());
                }
                current.clear();
                continue;
            }
            _ => {}
        }
        current.push(character);
    }
    if !current.trim().is_empty() {
        tools.push(current.trim().to_owned());
    }
    tools
}

/// `zcode --version` 的输出行（docs/specs/rust-cli/version.md §5）。
pub(crate) fn version_line(version: &str) -> String {
    format!("zcode {version} (rust-cli)")
}

/// JS `Number(text)`：去掉首尾空白；空串是 0；十进制（可带符号、小数、指数）、`Infinity`、`0x` / `0o` / `0b`。
/// 不是数字时为 None（NaN）。
pub(crate) fn js_number(text: &str) -> Option<f64> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Some(0.0);
    }
    for (prefix, radix) in [
        ("0x", 16),
        ("0X", 16),
        ("0o", 8),
        ("0O", 8),
        ("0b", 2),
        ("0B", 2),
    ] {
        if let Some(digits) = trimmed.strip_prefix(prefix) {
            #[expect(
                clippy::cast_precision_loss,
                reason = "与 JS 的数值相同，轮数不会大到丢精度"
            )]
            return u64::from_str_radix(digits, radix)
                .ok()
                .map(|value| value as f64);
        }
    }
    let unsigned = trimmed.strip_prefix(['+', '-']).unwrap_or(trimmed);
    if unsigned == "Infinity" {
        return Some(if trimmed.starts_with('-') {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        });
    }
    // Rust 认的 `inf`、`nan` 与 `1.` 之外的写法 JS 不认；只放行数字、小数点与指数。
    let plain = unsigned
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-'));
    if !plain || !unsigned.starts_with(|c: char| c.is_ascii_digit() || c == '.') {
        return None;
    }
    trimmed.parse::<f64>().ok()
}

/// `--max-turns` 的值怎样限制轮数：轮数大于它时停、为 0 时不限（JS 假值）。
/// 正数向下取整（2.5 与 2 一样在第 3 轮停）；0 不限；负数第一轮之后就停，这里记成 1（cli-args.md D9）。
fn max_turns(value: f64) -> Option<u32> {
    if value == 0.0 {
        return None;
    }
    if value < 1.0 {
        return Some(1);
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "已排除负数，超出 u32 时饱和"
    )]
    Some(value.floor().min(f64::from(u32::MAX)) as u32)
}

/// commander 的取值错误：`error: option '<flags>' argument '<值>' is invalid. <原因>`。
fn invalid(spec: &OptionSpec<Key>, value: &str, reason: &str) -> Stop {
    Stop::Error(format!(
        "error: option '{}' argument '{value}' is invalid. {reason}",
        spec.flags
    ))
}

/// UUID 校验（`--session-id`，cli-args.md §7）：8-4-4-4-12 的十六进制。
pub(crate) fn is_uuid(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(group, len)| group.len() == len && group.chars().all(|c| c.is_ascii_hexdigit()))
}

/// 一个选项出现时（commander 的事件）。
fn apply(
    invocation: &mut Invocation,
    resume: &mut Option<Option<String>>,
    continued: &mut bool,
    spec: &OptionSpec<Key>,
    value: Option<&str>,
) -> Result<(), Stop> {
    let text = || value.unwrap_or_default().to_owned();
    match spec.key {
        Key::AddDir => invocation.add_dirs.push(text()),
        Key::Allowed => invocation.allowed.extend(split_tools(&text())),
        Key::Disallowed => invocation.disallowed.extend(split_tools(&text())),
        Key::AppendSystemPrompt => invocation.append_system_prompt.inline = Some(text()),
        Key::AppendSystemPromptFile => invocation.append_system_prompt.file = Some(text()),
        Key::SystemPrompt => invocation.system_prompt.inline = Some(text()),
        Key::SystemPromptFile => invocation.system_prompt.file = Some(text()),
        Key::Continue => *continued = true,
        Key::Resume => *resume = Some(value.map(str::to_owned)),
        Key::ForkSession => invocation.fork_session = true,
        // 不认识的值不报错：记下警告，按缺省档位（后给的覆盖先给的）。
        Key::Effort => match effort::parse(&text()) {
            Ok(level) => invocation.effort = Some(level),
            Err(warning) => {
                invocation.effort = None;
                invocation.warnings.push(warning);
            }
        },
        Key::MaxTurns => {
            // JS `Number()` 的读法，NaN 时 `must be a number`（cli-args.md §2.2）。
            let value = text();
            let turns =
                js_number(&value).ok_or_else(|| invalid(spec, &value, "must be a number"))?;
            invocation.max_turns = max_turns(turns);
        }
        Key::McpConfig => invocation.mcp.configs.push(text()),
        Key::StrictMcpConfig => invocation.mcp.strict = true,
        Key::Model => invocation.model = Some(text()),
        Key::PermissionMode => {
            let value = text();
            let mode = permission_mode(&value).ok_or_else(|| {
                invalid(
                    spec,
                    &value,
                    &format!("Allowed choices are {MODE_CHOICES}."),
                )
            })?;
            invocation.permission_mode = Some(mode);
        }
        Key::PluginDir => invocation.plugin_dirs.push(text()),
        Key::Print => invocation.mode = Mode::Print,
        // 格式参数（headless.md §1，修复线 F39）：取值不在 choices 里时 commander 的原文。
        Key::OutputFormat => {
            let value = text();
            invocation.format.output = print_format::output_format(&value).ok_or_else(|| {
                invalid(spec, &value, "Allowed choices are text, json, stream-json.")
            })?;
        }
        Key::InputFormat => {
            let value = text();
            invocation.format.input = print_format::input_format(&value)
                .ok_or_else(|| invalid(spec, &value, "Allowed choices are text, stream-json."))?;
        }
        Key::Verbose => invocation.format.verbose = true,
        Key::IncludePartialMessages => invocation.format.partial = true,
        Key::SessionId => invocation.session_id = Some(text()),
        Key::Settings => invocation.settings = Some(text()),
        Key::Version => return Err(Stop::Version),
    }
    Ok(())
}

/// 解析命令行。错误是写到 stderr 的全文（退出码 1，commander 与参数校验都是 1）。
pub(crate) fn parse(args: &[String]) -> Result<Invocation, String> {
    // 入口的快速路径：只有一个参数、是 `--version` / `-v` / `-V` 时直接输出版本（version.md §5）。
    if let [only] = args
        && matches!(only.as_str(), "--version" | "-v" | "-V")
    {
        return Ok(Invocation {
            mode: Mode::Version,
            ..Invocation::default()
        });
    }
    let mut invocation = Invocation::default();
    let mut resume: Option<Option<String>> = None;
    let mut continued = false;
    let parsed = split(
        args,
        SPECS,
        |spec, value| apply(&mut invocation, &mut resume, &mut continued, spec, value),
        Stop::Error,
    );
    let parsed = match parsed {
        Ok(parsed) => parsed,
        Err(Stop::Version) => {
            return Ok(Invocation {
                mode: Mode::Version,
                ..Invocation::default()
            });
        }
        Err(Stop::Error(message)) => return Err(message),
    };
    // commander：帮助选项在不认识的参数里找（`_outputHelpIfRequested`），先于未知选项的报错。
    if parsed
        .unknown
        .iter()
        .any(|arg| arg == "-h" || arg == "--help")
    {
        return Ok(Invocation {
            mode: Mode::Help,
            ..Invocation::default()
        });
    }
    if let Some(first) = parsed.unknown.first() {
        let mut candidates = help::visible_longs();
        candidates.push("--help");
        return Err(format!(
            "error: unknown option '{first}'{}",
            suggestion(first, &candidates)
        ));
    }
    invocation.prompt = parsed.operands.into_iter().next();
    finish(invocation, resume, continued)
}

/// commander 之后、动作里做的、不读文件的校验（cli-args.md §2.4）。
fn finish(
    mut invocation: Invocation,
    resume: Option<Option<String>>,
    continued: bool,
) -> Result<Invocation, String> {
    let print = invocation.mode == Mode::Print;
    // 格式参数的组合（headless.md §1.1 第 2–4 条）；stream-json 输入时 `-p` 的 MCP prewait 可能是 `lifted`（§6.2）。
    print_format::check(invocation.format, print)?;
    invocation.mcp.streaming_input =
        invocation.format.input == rust_cli_headless::InputFormat::StreamJson;
    match (continued, resume) {
        (true, Some(_)) => {
            return Err("--continue and --resume cannot be used together".to_owned());
        }
        (true, None) => invocation.target = SessionTarget::Continue,
        (false, Some(Some(id))) => invocation.target = SessionTarget::Resume(SessionId::new(id)),
        // `--resume` 不带值时打开会话选择器（只有 TUI 有）；`-p` 里要 ID。
        (false, Some(None)) if print => {
            return Err("Error: --resume requires a valid session ID or session title when used with --print. Usage: zcode -p --resume <session-id|title>".to_owned());
        }
        (false, Some(None)) => invocation.resume_pick = true,
        (false, None) => {}
    }
    // 原来单独使用时报自拟的错误；正确的行为是没有这条校验（退出码 0、照常建新会话），所以
    // 单独使用时忽略它，避免 headless 去 fork 一个刚建的空会话（session-lifecycle.md §1.1）。
    invocation.fork_session = invocation.fork_session
        && (invocation.target != SessionTarget::New || invocation.resume_pick);
    if let Some(id) = &invocation.session_id {
        if !is_uuid(id) {
            return Err("Error: Invalid session ID. Must be a valid UUID.".to_owned());
        }
        if (invocation.target != SessionTarget::New || invocation.resume_pick)
            && !invocation.fork_session
        {
            return Err("Error: --session-id can only be used with --continue or --resume if --fork-session is also specified.".to_owned());
        }
    }
    Ok(invocation)
}

#[cfg(test)]
#[path = "args_tests.rs"]
mod tests;
