//! Node `evaluateBashReadonlyPolicy` and `hasKnownBashWriteOption`
//! (`bash-readonly-policy-argv*.ts`).
use super::{callbacks, callbacks_net, flags::argv_allowed, js, tables::tables};
use zcode_cli_bash_parse::Invocation;

const SAFE_ENV: [&str; 39] = [
    "ANTHROPIC_API_KEY",
    "BLOCK_SIZE",
    "BLOCKSIZE",
    "CGO_ENABLED",
    "CHARSET",
    "CI",
    "CLICOLOR",
    "CLICOLOR_FORCE",
    "COLORTERM",
    "COLUMNS",
    "DEBIAN_FRONTEND",
    "FORCE_COLOR",
    "GCC_COLORS",
    "GIT_TERMINAL_PROMPT",
    "GO111MODULE",
    "GOARCH",
    "GOEXPERIMENT",
    "GOOS",
    "GREP_COLOR",
    "GREP_COLORS",
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_CTYPE",
    "LC_TIME",
    "LINES",
    "LSCOLORS",
    "LS_COLORS",
    "NO_COLOR",
    "NODE_ENV",
    "PYTEST_DEBUG",
    "PYTEST_DISABLE_PLUGIN_AUTOLOAD",
    "PYTHONDONTWRITEBYTECODE",
    "PYTHONUNBUFFERED",
    "RUST_BACKTRACE",
    "RUST_LOG",
    "TERM",
    "TIME_STYLE",
    "TZ",
];

const FIND_WRITE: [&str; 10] = [
    "-delete",
    "-exec",
    "-execdir",
    "-files0-from",
    "-fls",
    "-fprint",
    "-fprint0",
    "-fprintf",
    "-ok",
    "-okdir",
];

const FIND_VALUE: [&str; 45] = [
    "-Bmin",
    "-Bnewer",
    "-Btime",
    "-D",
    "-amin",
    "-anewer",
    "-atime",
    "-cmin",
    "-cnewer",
    "-context",
    "-ctime",
    "-f",
    "-flags",
    "-fstype",
    "-gid",
    "-group",
    "-ilname",
    "-iname",
    "-inum",
    "-ipath",
    "-iregex",
    "-iwholename",
    "-lname",
    "-links",
    "-maxdepth",
    "-mindepth",
    "-mmin",
    "-mnewer",
    "-mtime",
    "-name",
    "-newer",
    "-path",
    "-perm",
    "-printf",
    "-regex",
    "-regextype",
    "-samefile",
    "-size",
    "-type",
    "-used",
    "-user",
    "-wholename",
    "-xattrname",
    "-xtype",
    "-uid",
];

const EXACT_ARGV: [&[&str]; 5] = [
    &["ip", "addr"],
    &["node", "-v"],
    &["node", "--version"],
    &["python", "--version"],
    &["python3", "--version"],
];

/// Node `stripSafeCommandWrappers`: `command [-p…] [--]`, `builtin [--]`, `noglob`.
pub(crate) fn strip_wrappers(argv: &[String]) -> &[String] {
    let mut words = argv;
    loop {
        match words.first().map(String::as_str) {
            Some("command") => {
                let mut index = 1;
                while words.get(index).is_some_and(|w| {
                    w.len() > 1 && w.starts_with('-') && w[1..].bytes().all(|b| b == b'p')
                }) {
                    index += 1;
                }
                if words.get(index).is_some_and(|w| w == "--") {
                    index += 1;
                }
                if index >= words.len() || words[index].starts_with('-') {
                    return words;
                }
                words = &words[index..];
            }
            Some("builtin") => {
                let index = if words.get(1).is_some_and(|w| w == "--") {
                    2
                } else {
                    1
                };
                if index >= words.len() {
                    return words;
                }
                words = &words[index..];
            }
            Some("noglob") if words.len() > 1 => words = &words[1..],
            _ => return words,
        }
    }
}

/// `/^(?:\/\/|\\\\)[^/\\]/`.
pub(crate) fn is_unc(value: &str) -> bool {
    let rest = value
        .strip_prefix("//")
        .or_else(|| value.strip_prefix("\\\\"));
    rest.and_then(|r| r.chars().next())
        .is_some_and(|c| c != '/' && c != '\\')
}

fn env_allowed(part: &Invocation) -> bool {
    part.env_assignments
        .iter()
        .all(|a| a.name.as_deref().is_some_and(|n| SAFE_ENV.contains(&n)))
}

fn redirects_allowed(part: &Invocation) -> bool {
    part.redirects.iter().all(|r| {
        let target = r.target.as_str();
        if target.starts_with("/dev/tcp/") || target.starts_with("/dev/udp/") {
            return false;
        }
        if r.operator == ">&" && js::is_digits(target) {
            return true;
        }
        if target == "/dev/null" {
            return true;
        }
        if matches!(r.operator.as_str(), "<" | "<<" | "<&" | "<<<") {
            return !is_unc(target);
        }
        false
    })
}

fn tree_output(argv: &[String]) -> bool {
    for word in &argv[1.min(argv.len())..] {
        if word.is_empty() {
            continue;
        }
        if word == "--" {
            return false;
        }
        if word == "-o" || word == "--output" || word.starts_with("--output=") {
            return true;
        }
        if word.starts_with('-') && !word.starts_with("--") && word[1..].contains('o') {
            return true;
        }
    }
    false
}

pub(crate) fn has_known_write_option(part: &Invocation) -> bool {
    let argv = strip_wrappers(&part.argv);
    match argv.first().map(String::as_str) {
        Some("sed") => argv.iter().any(|w| callbacks::is_sed_in_place(w)),
        Some("find") => argv.iter().any(|w| FIND_WRITE.contains(&w.as_str())),
        Some("tree") => tree_output(argv),
        Some("git") => argv.iter().any(|w| super::git::dangerous_global_word(w)),
        _ => false,
    }
}

/// Node `evaluateBashReadonlyPolicy`: `Some(true)` read-only, anything else not.
pub(crate) fn evaluate(part: &Invocation) -> Option<bool> {
    if !env_allowed(part) || !redirects_allowed(part) {
        return Some(false);
    }
    let argv = strip_wrappers(&part.argv);
    let Some(name) = argv.first() else {
        return Some(false);
    };
    if argv.iter().any(|w| is_unc(w)) {
        return Some(false);
    }
    if name == "git" {
        return Some(super::git::read_only(argv));
    }
    if let Some(result) = direct(argv) {
        return Some(result);
    }
    if let Some(result) = multiword(argv, &part.command_text) {
        return Some(result);
    }
    let tables = tables();
    if tables.allow_any.contains(name) {
        return Some(true);
    }
    if cfg!(windows) && name == "xargs" {
        return None;
    }
    let policy = tables.commands.get(name)?;
    if name == "cd" && argv.len() > 2 {
        return Some(false);
    }
    if policy.callback.is_some_and(|cb| cb(&argv[1..])) {
        return Some(false);
    }
    Some(
        argv_allowed(argv, policy, name, 1)
            && policy
                .regex
                .as_ref()
                .is_none_or(|re| re.is_match(&part.command_text)),
    )
}

fn starts_with_words(argv: &[String], words: &[impl AsRef<str>]) -> bool {
    argv.len() >= words.len() && words.iter().zip(argv).all(|(w, a)| w.as_ref() == a)
}

fn direct(argv: &[String]) -> Option<bool> {
    let name = argv[0].as_str();
    if EXACT_ARGV
        .iter()
        .any(|e| e.len() == argv.len() && starts_with_words(argv, e))
    {
        return Some(true);
    }
    if name == "docker"
        && tables()
            .allow_any_prefixes
            .iter()
            .any(|p| starts_with_words(argv, p))
    {
        return Some(!callbacks_net::has_dangerous_docker_option(argv));
    }
    let second = argv.get(1).map(String::as_str);
    match name {
        "printf" => Some(super::printf::safe(argv)),
        "find" => Some(find_safe(argv)),
        "history" => {
            Some(argv.len() == 1 || (argv.len() == 2 && second.is_some_and(js::is_digits)))
        }
        "arch" => {
            Some(argv.len() == 1 || (argv.len() == 2 && matches!(second, Some("-h" | "--help"))))
        }
        "ifconfig" => Some(
            argv.len() == 1
                || (argv.len() == 2
                    && second.is_some_and(|s| {
                        s.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
                    })),
        ),
        _ => None,
    }
}

fn find_safe(argv: &[String]) -> bool {
    let mut index = 1;
    while index < argv.len() {
        let word = argv[index].as_str();
        if FIND_WRITE.contains(&word) {
            return false;
        }
        // `/^-newer[aBcm][aBcmt]$/` 取值。
        let newer = word.strip_prefix("-newer").is_some_and(|r| {
            let r = r.as_bytes();
            r.len() == 2 && b"aBcm".contains(&r[0]) && b"aBcmt".contains(&r[1])
        });
        if FIND_VALUE.contains(&word) || newer {
            index += 1;
        }
        index += 1;
    }
    true
}

/// Multiword prefixes (`docker inspect`, `gh pr view`, …), longest first.
fn multiword(argv: &[String], command_text: &str) -> Option<bool> {
    let policy = tables()
        .multiword
        .iter()
        .find(|p| starts_with_words(argv, &p.words))?;
    let args = &argv[policy.words.len()..];
    // 前缀之后的参数含 `$` 或花括号展开形态时直接判非只读。
    let unsafe_text = args
        .iter()
        .any(|a| a.contains('$') || (a.contains('{') && (a.contains(',') || a.contains(".."))));
    if unsafe_text || policy.callback.is_some_and(|cb| cb(args)) {
        return Some(false);
    }
    Some(
        argv_allowed(argv, policy, &argv[0], policy.words.len())
            && policy
                .regex
                .as_ref()
                .is_none_or(|re| re.is_match(command_text)),
    )
}
