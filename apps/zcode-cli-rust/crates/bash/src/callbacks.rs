//! Dangerous-argument callbacks (`bash-readonly-policy-callbacks.ts`), bound by
//! the names the exported tables use.
use super::{git_callbacks, js};

pub(crate) const XARGS_TARGETS: [&str; 8] = [
    "echo", "printf", "wc", "grep", "egrep", "fgrep", "head", "tail",
];

pub(crate) fn by_name(name: &str) -> Option<super::tables::Callback> {
    Some(match name {
        "dateCommandIsDangerous" => date,
        "jqCommandIsDangerous" => jq,
        "lsofCommandIsDangerous" => lsof,
        "manCommandIsDangerous" => man,
        "psCommandIsDangerous" => ps,
        "pyrightCommandIsDangerous" => |args| args.iter().any(|a| a == "--watch" || a == "-w"),
        "sedCommandIsDangerous" => sed,
        "ssCommandIsDangerous" => super::callbacks_net::ss,
        "testCommandIsDangerous" => test,
        "tputCommandIsDangerous" => tput,
        "xargsCommandIsDangerous" => xargs,
        "ghCommandIsDangerous" => super::callbacks_net::gh,
        "dockerCommandIsDangerous" => super::callbacks_net::docker,
        other => return git_callbacks::by_name(other),
    })
}

pub(crate) fn is_sed_in_place(word: &str) -> bool {
    word.starts_with("-i") || word == "--in-place" || word.starts_with("--in-place=")
}

fn jq(args: &[String]) -> bool {
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        index += 1;
        if arg.is_empty() {
            continue;
        }
        if jq_dangerous_option(arg) {
            return true;
        }
        if arg == "--" {
            return jq_filter_dangerous(args.get(index).map_or("", String::as_str));
        }
        if arg == "--indent" {
            index += 1;
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        return jq_filter_dangerous(arg);
    }
    false
}

fn jq_dangerous_option(word: &str) -> bool {
    word.starts_with("-f")
        || word.starts_with("-L")
        || [
            "--argfile",
            "--from-file",
            "--library-path",
            "--rawfile",
            "--run-tests",
            "--slurpfile",
        ]
        .iter()
        .any(|o| word == *o || word.strip_prefix(o).is_some_and(|r| r.starts_with('=')))
}

fn word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// `$ENV\b`, `(^|[^A-Za-z0-9_$.])env(?=$|[^A-Za-z0-9_])`,
/// `(^|[^A-Za-z0-9_])(?:include|import)(?=$|[^A-Za-z0-9_])` (ASCII classes).
fn jq_filter_dangerous(filter: &str) -> bool {
    let bytes = filter.as_bytes();
    let found = |needle: &str, before: &dyn Fn(u8) -> bool, boundary_before: bool| {
        filter.match_indices(needle).any(|(at, _)| {
            let after = bytes.get(at + needle.len());
            let before_ok = at == 0 || before(bytes[at - 1]);
            let after_ok = after.is_none_or(|b| !word_byte(*b));
            (before_ok || !boundary_before) && after_ok
        })
    };
    // `\b` 之后要求非单词字符或结尾；`$ENV` 前面不设限制。
    found("$ENV", &|_| true, false)
        || found("env", &|b| !(word_byte(b) || b == b'$' || b == b'.'), true)
        || found("include", &|b| !word_byte(b), true)
        || found("import", &|b| !word_byte(b), true)
}

fn sed(args: &[String]) -> bool {
    let mut first_script = false;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        index += 1;
        if arg.is_empty() {
            continue;
        }
        if is_sed_in_place(arg) {
            return true;
        }
        if arg == "-e" || arg == "--expression" {
            if sed_writes(args.get(index).map_or("", String::as_str)) {
                return true;
            }
            index += 1;
            continue;
        }
        if let Some(script) = arg.strip_prefix("--expression=") {
            if sed_writes(script) {
                return true;
            }
            continue;
        }
        if arg == "-l" || arg == "--line-length" {
            index += 1;
            continue;
        }
        if arg.starts_with("--line-length=") {
            continue;
        }
        if arg == "--" {
            return sed_writes(args.get(index).map_or("", String::as_str));
        }
        if !arg.starts_with('-') && !first_script {
            first_script = true;
            if sed_writes(arg) {
                return true;
            }
        }
    }
    false
}

/// `/(?:^|[;{\n])\s*(?:[0-9,$!+~-]+)?\s*w(?:\s|$)/` — a `w` command. The
/// `s///w file` flag is not caught (kept from Node).
fn sed_writes(script: &str) -> bool {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| js::regex(r"(?:^|[;{\n])\s*(?:[0-9,$!+~-]+)?\s*w(?:\s|$)"))
        .is_match(script)
}

fn date(args: &[String]) -> bool {
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        if arg.starts_with("--") && arg.contains('=') {
            index += 1;
            continue;
        }
        if arg.starts_with('-') {
            let value = matches!(arg, "-d" | "--date" | "-r" | "--reference" | "--rfc-3339");
            index += if value { 2 } else { 1 };
            continue;
        }
        if !arg.starts_with('+') {
            return true;
        }
        index += 1;
    }
    false
}

/// BSD `e` prints the environment.
fn ps(args: &[String]) -> bool {
    args.iter().any(|a| {
        !a.starts_with('-') && a.contains('e') && a.bytes().all(|b| b.is_ascii_alphabetic())
    })
}

fn man(args: &[String]) -> bool {
    let mut apropos = false;
    let mut after_double_dash = false;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        index += 1;
        if !after_double_dash && arg == "--" {
            after_double_dash = true;
            continue;
        }
        if !after_double_dash && arg.starts_with('-') && arg != "-" {
            if matches!(arg, "-k" | "-f" | "--apropos" | "--whatis") {
                apropos = true;
            }
            if matches!(arg, "-S" | "-s") {
                index += 1;
            }
            continue;
        }
        after_double_dash = true;
        if arg.contains('/') {
            return !apropos;
        }
    }
    false
}

fn lsof(args: &[String]) -> bool {
    let host_has_letter = |word: &str| {
        let host = &word[word.find('@').map_or(0, |i| i + 1)..];
        let host = host.split(':').next().unwrap_or("");
        host.bytes().any(|b| b.is_ascii_alphabetic())
    };
    for (index, arg) in args.iter().enumerate() {
        if arg.starts_with("+m") {
            return true;
        }
        // `/^-[a-zA-Z]*i\S*@/` 与 `/^-[a-zA-Z]*i$/`。
        let Some(rest) = arg.strip_prefix('-') else {
            continue;
        };
        let letters = rest.bytes().take_while(u8::is_ascii_alphabetic).count();
        let attached = rest[..letters].char_indices().any(|(i, c)| {
            c == 'i' && {
                let tail = &rest[i + 1..];
                let end = tail.find(js::is_space).unwrap_or(tail.len());
                tail[..end].contains('@')
            }
        });
        if attached && host_has_letter(arg) {
            return true;
        }
        if letters == rest.len()
            && rest.ends_with('i')
            && let Some(next) = args.get(index + 1)
            && next.contains('@')
            && host_has_letter(next)
        {
            return true;
        }
    }
    false
}

const TPUT_DANGEROUS: [&str; 24] = [
    "clear", "flash", "if", "init", "iprog", "is1", "is2", "is3", "mc0", "mc4", "mc5", "mc5i",
    "mc5p", "pfkey", "pfloc", "pfx", "pfxl", "reset", "rf", "rmcup", "rs1", "rs2", "rs3", "smcup",
];

fn tput(args: &[String]) -> bool {
    let mut after_double_dash = false;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        if arg == "--" {
            after_double_dash = true;
            index += 1;
            continue;
        }
        if !after_double_dash && arg.starts_with('-') {
            if arg == "-S"
                || (!arg.starts_with("--") && js::utf16_len(arg) > 2 && arg.contains('S'))
            {
                return true;
            }
            index += if arg == "-T" { 2 } else { 1 };
            continue;
        }
        if TPUT_DANGEROUS.contains(&arg) {
            return true;
        }
        index += 1;
    }
    false
}

/// `/^-?(0[xX][0-9a-fA-F]+|[0-9]+#[0-9a-zA-Z]+|[0-9]+)$/`.
fn test_safe_number(value: &str) -> bool {
    let v = value.strip_prefix('-').unwrap_or(value);
    let hex = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit());
    if let Some(h) = v.strip_prefix("0x").or_else(|| v.strip_prefix("0X"))
        && hex(h)
    {
        return true;
    }
    if let Some((base, digits)) = v.split_once('#') {
        return js::is_digits(base)
            && !digits.is_empty()
            && digits.bytes().all(|b| b.is_ascii_alphanumeric());
    }
    js::is_digits(v)
}

fn test(args: &[String]) -> bool {
    if args
        .iter()
        .any(|a| matches!(a.as_str(), "-v" | "-R" | "-a" | "-o") || a.contains('['))
    {
        return true;
    }
    for (index, arg) in args.iter().enumerate() {
        if matches!(arg.as_str(), "-eq" | "-ne" | "-lt" | "-le" | "-gt" | "-ge") {
            let before = index.checked_sub(1).and_then(|i| args.get(i));
            for value in [before, args.get(index + 1)].into_iter().flatten() {
                if !test_safe_number(value) {
                    return true;
                }
            }
        }
        if arg == "-t"
            && let Some(value) = args.get(index + 1)
            && !test_safe_number(value)
        {
            return true;
        }
    }
    false
}

fn xargs(args: &[String]) -> bool {
    let mut index = 0;
    while index < args.len() {
        let mut arg = args[index].as_str();
        if arg.is_empty() {
            index += 1;
            continue;
        }
        if arg == "--" && index + 1 < args.len() {
            index += 1;
            arg = args[index].as_str();
        }
        if arg.starts_with('-') && arg != "-" {
            if matches!(arg, "-I" | "-n" | "-P" | "-L" | "-s" | "-E" | "-d") {
                index += 1;
            }
            index += 1;
            continue;
        }
        return !XARGS_TARGETS.contains(&arg);
    }
    false
}
