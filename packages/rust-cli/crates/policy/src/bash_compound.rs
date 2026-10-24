//! 复合结构（shell-permissions.md §4.5.4，工作线 F76，用户决定 2026-10-23“拆开逐段套规则”）。
//!
//! Node 对齐的解析在复合结构处停下（§4.3）；这时改用结构化的分析，把 if / while / for 的条件与循环体、子 shell
//! 里的列表、`[[ … ]]` 拆成简单命令逐段检查，并给出结构的事实（有没有子 shell、顶层有没有 `&`）。拆不开的写法
//! 整条不能静态检查，按种类给出理由。以前（F68）这些命令整段只认精确的 allow 规则，`Bash(echo:*)` 下
//! `if true; then echo yes; fi` 也询问。

use std::ops::Range;

use crate::bash_subst::{self, AS_FILE, Expanded, NESTED};
use crate::parse::{Analysis, Complex, Invocation, Loop, analyze_structured};

/// 后台 `&`：每段都放行时仍询问一次（安全检查，bypass 放行、allow 规则盖不过）。
pub(crate) const BACKGROUND: &str = "This command uses the `&` background operator, which defers execution past approval-time safety checks. Approve only if you trust it.";
/// 有子 shell 时的询问（deny 规则与危险删除之后）。
pub(crate) const SHELL_OPERATORS: &str =
    "This command uses shell operators that require approval for safety";

/// 拆的结果。
pub(crate) enum Split {
    /// 解析不了（缺 `fi` 等）：整条当一个，只认精确的 allow 规则。
    Whole,
    /// 子命令（替换里拆出的命令排在外层命令前面）与结构的事实。
    Parts {
        parts: Vec<Invocation>,
        /// 每一段被拒绝时文字里的命令（§4.5.5）：`None` 是整条命令；有管道（没有子 shell）时是它所在的那一截。
        named: Vec<Option<String>>,
        /// 每一段所在的那一截管道（下标）与各截的原文（去首尾空白；没有管道时一截，是整条命令）：cd 按截处理（§4.5.7）。
        groups: Vec<usize>,
        group_texts: Vec<String>,
        subshell: bool,
        background: bool,
    },
    /// 整条不能静态检查：询问的理由与找到的简单命令（deny 规则与危险删除逐个看）。
    Complex {
        reason: String,
        commands: Vec<Invocation>,
    },
}

/// 按 §4.5.2、§4.5.4 拆开一条命令；`analysis` 是 Node 对齐的分析。
pub(crate) fn split(command: &str, analysis: &Analysis) -> Split {
    let stopped = analysis.has_unsupported_syntax && !analysis.has_parse_errors;
    if !stopped {
        if analysis.has_parse_errors || analysis.commands.is_empty() {
            return Split::Whole;
        }
        return expanded(command, analysis, analysis.commands.clone(), false, false);
    }
    let Some(mut structured) = analyze_structured(command) else {
        return Split::Whole;
    };
    let commands = std::mem::take(&mut structured.analysis.commands);
    let shape = structured.shape;
    let mut complex = shape.complex;
    for item in &shape.loops {
        complex.extend(loop_variable(command, item));
    }
    if let Some((pos, kind)) = complex.into_iter().min_by_key(|(pos, _)| *pos) {
        // 不能拆的替换在前时用替换的理由（按原文的顺序，先遇到哪个用哪个）。
        let before: Vec<Invocation> = commands
            .iter()
            .filter(|invocation| invocation.pos < pos)
            .cloned()
            .collect();
        let reason = match bash_subst::expand(&before) {
            Expanded::Complex(reason) => reason.to_owned(),
            Expanded::Parts(_) => complex_reason(&kind),
        };
        return Split::Complex { reason, commands };
    }
    if commands.is_empty() {
        return Split::Whole;
    }
    expanded(
        command,
        &structured.analysis,
        commands,
        shape.subshell,
        shape.background,
    )
}

/// 拆开替换（§4.5.2），记下每一段属于哪一条命令（它在原文里的起点），按它取被拒绝时的文字（§4.5.5）。
/// `spans` 是记下管道符与输出重定向位置的那次分析。
fn expanded(
    command: &str,
    spans: &Analysis,
    commands: Vec<Invocation>,
    subshell: bool,
    background: bool,
) -> Split {
    let mut parts = Vec::new();
    let mut owners = Vec::new();
    // 逐条拆，与整张表一起拆的结果相同（替换里的命令排在它所在的命令前面，遇到不能拆的就停）。
    for invocation in &commands {
        match bash_subst::expand(std::slice::from_ref(invocation)) {
            Expanded::Parts(found) => {
                parts.extend(found);
                owners.resize(parts.len(), invocation.pos);
            }
            Expanded::Complex(reason) => {
                return Split::Complex {
                    reason: reason.to_owned(),
                    commands,
                };
            }
        }
    }
    // 有子 shell 时整条命令先按 shell operators 处理，不按管道分段。
    let piped = !subshell && !spans.pipes.is_empty();
    let named = owners
        .iter()
        .map(|&at| piped.then(|| segment(command, &spans.pipes, &spans.outputs, at)))
        .collect();
    let mut pipes: Vec<std::ops::Range<usize>> = spans.pipes.clone();
    pipes.sort_by_key(|pipe| pipe.start);
    let groups = owners
        .iter()
        .map(|&at| pipes.iter().filter(|pipe| pipe.end <= at).count())
        .collect();
    let mut group_texts = Vec::with_capacity(pipes.len() + 1);
    let mut start = 0;
    for pipe in pipes
        .iter()
        .chain(std::iter::once(&(command.len()..command.len())))
    {
        group_texts.push(
            command
                .get(start..pipe.start)
                .unwrap_or(command)
                .trim()
                .to_owned(),
        );
        start = pipe.end;
    }
    Split::Parts {
        parts,
        named,
        groups,
        group_texts,
        subshell,
        background,
    }
}

/// `at` 所在的那一截管道（§4.5.5）：管道符之间的原文去首尾空白；里面有 `>` / `>>` 重定向时删掉它们，空格与
/// 制表符并成一个（再去掉换行两边的空白）。位置都在 ASCII 字符上；切不开时退回整条命令。
fn segment(command: &str, pipes: &[Range<usize>], outputs: &[Range<usize>], at: usize) -> String {
    let start = pipes
        .iter()
        .filter(|pipe| pipe.end <= at)
        .map(|pipe| pipe.end)
        .max()
        .unwrap_or(0);
    let end = pipes
        .iter()
        .filter(|pipe| pipe.start >= at)
        .map(|pipe| pipe.start)
        .min()
        .unwrap_or(command.len());
    let mut removed: Vec<&Range<usize>> = outputs
        .iter()
        .filter(|output| output.start >= start && output.end <= end)
        .collect();
    removed.sort_by_key(|output| output.start);
    let Some(text) = command.get(start..end) else {
        return command.to_owned();
    };
    if removed.is_empty() {
        return text.trim().to_owned();
    }
    let mut kept = String::new();
    let mut cursor = start;
    for output in removed {
        let Some(piece) = command.get(cursor..output.start.max(cursor)) else {
            return command.to_owned();
        };
        kept.push_str(piece);
        cursor = cursor.max(output.end);
    }
    kept.push_str(command.get(cursor..end).unwrap_or_default());
    let mut collapsed = String::with_capacity(kept.len());
    for ch in kept.chars() {
        let blank = matches!(ch, ' ' | '\t');
        if blank && collapsed.ends_with(' ') {
            continue;
        }
        collapsed.push(if blank { ' ' } else { ch });
    }
    collapsed
        .replace(" \n", "\n")
        .replace("\n ", "\n")
        .trim()
        .to_owned()
}

/// 不能拆的写法的询问文字（逐字固定，出处见 provenance/policy.md 的 F76）。
fn complex_reason(kind: &Complex) -> String {
    let what = match kind {
        Complex::Generic => return "This command can't be checked before it runs".to_owned(),
        Complex::Select => {
            return "select statement reads stdin into $REPLY; cannot statically model".to_owned();
        }
        Complex::Nested => return NESTED.to_owned(),
        Complex::AsFile => return AS_FILE.to_owned(),
        Complex::LoopVariable(name) => {
            return format!("{name} as loop variable bypasses assignment validation");
        }
        Complex::Subshell => "A group in parentheses",
        Complex::Braces => "A group in braces or double parentheses",
        Complex::For => "A for or select loop",
        Complex::CountingFor => "A counting for loop",
        Complex::While => "A while or until loop",
        Complex::If => "An if statement",
        Complex::Case => "A case statement",
        Complex::Function => "A function definition",
        Complex::Test => "A test in brackets",
        Complex::Variable => "A variable",
        Complex::VariableBraces => "A variable in braces",
        Complex::BracePattern => "A brace pattern",
    };
    format!("{what} in this command can't be checked before it runs")
}

/// 循环变量：会影响执行或由 shell 维护的名字不能拿来当循环变量；之后引用它（值不知道）也不能拆。
fn loop_variable(command: &str, item: &Loop) -> Option<(usize, Complex)> {
    let name = String::from_utf8_lossy(&item.name).into_owned();
    if special(&name) {
        return Some((item.pos, Complex::LoopVariable(name)));
    }
    referenced(command.as_bytes(), &item.name, item.body).map(|at| (at, Complex::Variable))
}

/// 原文 `from` 之后第一处 `$NAME` / `${NAME…}`（名字后面不再接名字的字符；转义的 `\$` 不算）。
fn referenced(source: &[u8], name: &[u8], from: usize) -> Option<usize> {
    let word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    let mut index = from;
    while index < source.len() {
        if source[index] == b'\\' {
            index += 2;
            continue;
        }
        if source[index] == b'$' {
            let start = index + 1 + usize::from(source.get(index + 1) == Some(&b'{'));
            let end = start + name.len();
            if source.get(start..end) == Some(name) && !source.get(end).copied().is_some_and(word) {
                return Some(index);
            }
        }
        index += 1;
    }
    None
}

/// 不许当循环变量的名字：会影响执行（`PS4`、`IFS`、`PATH`、`LD_*` 等）或由 shell 维护的变量（出处见 provenance/policy.md 的 F76）。
fn special(name: &str) -> bool {
    const EXACT: &[&str] = &[
        "PS4",
        "IFS",
        "SHELLOPTS",
        "BASHOPTS",
        "BASH_VERSINFO",
        "BASH_ARGV0",
    ];
    // 小写比较的这张表，加上以 ld_、dyld_、bash_func_ 开头的名字。
    const LOWER: &[&str] = &[
        "path",
        "home",
        "dirstack",
        "tmpprefix",
        "bash_env",
        "env",
        "cdpath",
        "globignore",
        "shell",
        "fpath",
        "bash_loadables_path",
        "module_path",
        "manpath",
        "mailpath",
        "readnullcmd",
        "nullcmd",
        "histfile",
        "zdotdir",
        "functions",
        "commands",
        "aliases",
        "galiases",
        "saliases",
        "bash_cmds",
        "bash_aliases",
        "execignore",
        "bash_source_path",
        "lang",
        "language",
        "lc_all",
        "lc_ctype",
        "lc_collate",
        "lc_messages",
        "lc_numeric",
        "lc_time",
        "histchars",
        "textdomain",
        "textdomaindir",
    ];
    // shell 维护或带特殊含义的变量。
    const SHELL: &[&str] = &[
        "RANDOM",
        "SECONDS",
        "LINENO",
        "OPTIND",
        "MAILCHECK",
        "HISTCMD",
        "SRANDOM",
        "EPOCHSECONDS",
        "EPOCHREALTIME",
        "BASHPID",
        "BASH_MONOSECONDS",
        "BASH_TRAPSIG",
        "COLUMNS",
        "LINES",
        "SHLVL",
        "ERRNO",
        "TMOUT",
        "HISTSIZE",
        "SAVEHIST",
        "TRY_BLOCK_ERROR",
        "TRY_BLOCK_INTERRUPT",
        "KEYTIMEOUT",
        "LISTMAX",
        "LOGCHECK",
        "PERIOD",
        "FUNCNEST",
        "UID",
        "EUID",
        "GID",
        "EGID",
        "REPORTTIME",
        "REPORTMEMORY",
        "DIRSTACKSIZE",
        "BAUD",
        "ZLE_RPROMPT_INDENT",
        "MBEGIN",
        "MEND",
        "PPID",
        "ARGC",
        "ZSH_SUBSHELL",
        "TTYIDLE",
        "status",
        "HOME",
        "PWD",
        "OLDPWD",
        "USER",
        "LOGNAME",
        "SHELL",
        "PATH",
        "HOSTNAME",
        "TMPDIR",
        "BASH_VERSION",
        "HISTFILE",
        "_",
        "BASH_COMMAND",
        "FUNCNAME",
        "REPLY",
        "reply",
        "PIPESTATUS",
        "pipestatus",
        "BASH_SOURCE",
        "DIRSTACK",
        "GROUPS",
        "BASH_ARGV",
        "BASH_ARGC",
        "BASH_SUBSHELL",
        "BASH_LINENO",
        "BASH_REMATCH",
        "MATCH",
        "match",
        "mbegin",
        "mend",
        "OPTARG",
        "argv",
        "FIGNORE",
        "fignore",
        "PSVAR",
        "psvar",
        "WATCH",
        "watch",
        "HISTCHARS",
        "histchars",
        "PS1",
        "PROMPT",
        "prompt",
        "PS2",
        "PROMPT2",
        "PS3",
        "PROMPT3",
        "PROMPT4",
        "RPS1",
        "RPROMPT",
        "RPS2",
        "RPROMPT2",
    ];
    let lower = name.to_ascii_lowercase();
    EXACT.contains(&name)
        || SHELL.contains(&name)
        || LOWER.contains(&lower.as_str())
        || ["ld_", "dyld_", "bash_func_"]
            .iter()
            .any(|prefix| lower.starts_with(prefix))
}

/// `[[ … ]]` 的只读判定（按 argv）：`-v` / `-R` / `-t` 的操作数不含 `[`，`-t` 的操作数是数字，
/// `-eq` 等算术比较两边是数字。有动态词或重定向时不算只读（D18）。不是 `[[` 命令时为假。
pub(crate) fn test_is_read_only(invocation: &Invocation) -> bool {
    if invocation.name != "[[" || !invocation.command_text.starts_with("[[") {
        return false;
    }
    if invocation.has_dynamic_words || invocation.has_redirects {
        return false;
    }
    const ARITHMETIC: [&str; 6] = ["-eq", "-ne", "-lt", "-le", "-gt", "-ge"];
    let argv = &invocation.argv;
    argv.iter().enumerate().skip(1).all(|(index, arg)| {
        let next = argv.get(index + 1);
        let bracket = |value: Option<&String>| value.is_some_and(|value| value.contains('['));
        if matches!(arg.as_str(), "-v" | "-R" | "-t") && bracket(next) {
            return false;
        }
        if arg == "-t" && next.is_some_and(|value| !numeric(value)) {
            return false;
        }
        if ARITHMETIC.contains(&arg.as_str()) {
            return [argv.get(index - 1), next]
                .into_iter()
                .flatten()
                .all(|side| !side.contains('[') && numeric(side));
        }
        true
    })
}

/// `/^-?(0[xX][0-9a-fA-F]+|[0-9]+#[0-9a-zA-Z]+|[0-9]+)$/`。
fn numeric(value: &str) -> bool {
    let value = value.strip_prefix('-').unwrap_or(value);
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .filter(|hex| !hex.is_empty())
    {
        return hex.bytes().all(|byte| byte.is_ascii_hexdigit());
    }
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    match value.split_once('#') {
        Some((base, rest)) => {
            digits(base)
                && !rest.is_empty()
                && rest.bytes().all(|byte| byte.is_ascii_alphanumeric())
        }
        None => digits(value),
    }
}
