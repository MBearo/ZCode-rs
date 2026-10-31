//! argv 层去掉包装命令（shell-permissions.md §4.5.6，工作线 F85）：只给 deny 与 ask 规则的候选用。
//!
//! 文字层的 [`crate::shell_strip`] 只认五个包装命令与 `command` / `builtin` / `noglob`；这里按解析出的 argv 再去掉
//! `env`、`sudo`、`exec`、`watch` 等 20 个（跳过它们的选项），`env -S`、`flock -c`、`script -c` 的值当命令文字继续拆。
//! 选项表照对照实现（出处见 provenance/policy.md 的 F85）。

use std::sync::OnceLock;

use regex::Regex;

use crate::consts::{NICE_DASH_N_WORDS, OPTION_WITH_VALUE_WORDS, SHORT_OPTION_LEN};

/// 包装命令与它们带值的选项（值跟在后面一个词里）。
const WITH_VALUES: &[(&str, &[&str])] = &[
    ("env", &["-u", "-C", "--unset", "--chdir"]),
    (
        "sudo",
        &[
            "-u",
            "-g",
            "-U",
            "-C",
            "-D",
            "-h",
            "-p",
            "-r",
            "-R",
            "-t",
            "-T",
            "--user",
            "--group",
            "--other-user",
            "--close-from",
            "--chdir",
            "--host",
            "--prompt",
            "--role",
            "--chroot",
            "--type",
            "--command-timeout",
            "-a",
            "--auth-type",
        ],
    ),
    ("doas", &["-a", "-u", "-C"]),
    ("pkexec", &["--user"]),
    ("watch", &["-n", "--interval", "--equexit"]),
    (
        "ionice",
        &[
            "-c",
            "-n",
            "-p",
            "-P",
            "-u",
            "--class",
            "--classdata",
            "--pid",
            "--pgid",
            "--uid",
        ],
    ),
    ("setsid", &[]),
    ("taskset", &["-c", "--cpu-list"]),
    (
        "chrt",
        &[
            "-p",
            "--pid",
            "-T",
            "-P",
            "-D",
            "--sched-runtime",
            "--sched-period",
            "--sched-deadline",
        ],
    ),
    (
        "strace",
        &[
            "-e",
            "-o",
            "-p",
            "-s",
            "-E",
            "-P",
            "-S",
            "-a",
            "-b",
            "-I",
            "-u",
            "-X",
            "-O",
            "-U",
            "--output",
            "--trace",
            "--expr",
            "--attach",
            "--string-limit",
            "--env",
            "--trace-path",
            "--columns",
            "--user",
            "--interruptible",
            "--detach-on",
            "--const-print-style",
            "--summary-sort-by",
            "--summary-syscall-overhead",
            "--summary-columns",
        ],
    ),
    (
        "ltrace",
        &[
            "-a",
            "-A",
            "-e",
            "-l",
            "-n",
            "-o",
            "-p",
            "-s",
            "-u",
            "-x",
            "-D",
            "-F",
            "--align",
            "--config",
            "--debug",
            "--indent",
            "--library",
            "--output",
            "--string-max",
            "-w",
            "--where",
        ],
    ),
    (
        "flock",
        &["-w", "-E", "--timeout", "--wait", "--conflict-exit-code"],
    ),
    (
        "script",
        &[
            "-E",
            "-T",
            "-m",
            "-o",
            "-O",
            "-B",
            "-I",
            "--echo",
            "--log-timing",
            "--logging-format",
            "--output-limit",
            "--log-out",
            "--log-io",
            "--log-in",
        ],
    ),
    (
        "unshare",
        &[
            "-R",
            "-w",
            "-S",
            "-G",
            "--setuid",
            "--setgid",
            "--root",
            "--wd",
            "--propagation",
            "--setgroups",
            "--monotonic",
            "--boottime",
        ],
    ),
    (
        "nsenter",
        &["-t", "-S", "-G", "--target", "--setuid", "--setgid"],
    ),
    ("exec", &["-a"]),
    ("command", &[]),
    ("builtin", &[]),
    ("noglob", &[]),
    ("nocorrect", &[]),
];

/// 值是命令文字的选项。
fn command_options(name: &str) -> &'static [&'static str] {
    match name {
        "env" => &["-S", "--split-string"],
        "flock" | "script" => &["-c", "--command"],
        _ => &[],
    }
}

/// 包装命令后面的一个位置参数不是命令（优先级、CPU 掩码、锁文件、记录文件）。
fn positional(name: &str, word: &str) -> Option<bool> {
    match name {
        "chrt" => Some(!word.is_empty() && word.bytes().all(|b| b.is_ascii_digit())),
        "taskset" => {
            let lower = word.to_ascii_lowercase();
            let hex = lower.strip_prefix("0x").is_some_and(|rest| {
                !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_hexdigit())
            });
            Some(hex || (!word.is_empty() && word.bytes().all(|b| b.is_ascii_digit())))
        }
        "flock" | "script" => Some(true),
        _ => None,
    }
}

struct Patterns {
    assignment: Regex,
    flag_value: Regex,
    long_flag: Regex,
    fused_flag: Regex,
    duration: Regex,
    niceness: Regex,
}

fn patterns() -> Option<&'static Patterns> {
    static PATTERNS: OnceLock<Option<Patterns>> = OnceLock::new();
    PATTERNS
        .get_or_init(|| {
            Some(Patterns {
                assignment: Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*\+?=").ok()?,
                flag_value: Regex::new(r"^[A-Za-z0-9_.+-]+$").ok()?,
                long_flag: Regex::new(r"^--(?:kill-after|signal)=[A-Za-z0-9_.+-]+$").ok()?,
                fused_flag: Regex::new(r"^-[ks][A-Za-z0-9_.+-]+$").ok()?,
                duration: Regex::new(r"^\d+(?:\.\d+)?[smhd]?$").ok()?,
                niceness: Regex::new(r"^-?\d+$").ok()?,
            })
        })
        .as_ref()
}

/// `timeout` 的选项之后时长所在的下标；选项认不出时 None。
fn timeout_duration(argv: &[String], patterns: &Patterns) -> Option<usize> {
    let value = |word: Option<&String>| {
        word.is_some_and(|word| !word.is_empty() && patterns.flag_value.is_match(word))
    };
    let mut at = 1;
    while let Some(word) = argv.get(at) {
        let word = word.as_str();
        if matches!(word, "--foreground" | "--preserve-status" | "--verbose")
            || patterns.long_flag.is_match(word)
        {
            at += 1;
        } else if matches!(word, "--kill-after" | "--signal") && value(argv.get(at + 1)) {
            at += OPTION_WITH_VALUE_WORDS;
        } else if word == "--" {
            at += 1;
            break;
        } else if word.starts_with("--") {
            return None;
        } else if word == "-v" {
            at += 1;
        } else if matches!(word, "-k" | "-s") && value(argv.get(at + 1)) {
            at += OPTION_WITH_VALUE_WORDS;
        } else if patterns.fused_flag.is_match(word) {
            at += 1;
        } else if word.starts_with('-') {
            return None;
        } else {
            break;
        }
    }
    Some(at)
}

/// 前缀占到 `at` 为止；`at` 处是单独的 `--` 时它也算前缀。
fn skip_dashes(at: usize, argv: &[String]) -> usize {
    if argv.get(at).is_some_and(|word| word == "--") {
        at + 1
    } else {
        at
    }
}

/// `time`、`nohup`、`timeout <选项> <时长>`、`nice -n N`。
fn strip_runners(mut argv: Vec<String>, patterns: &Patterns) -> Vec<String> {
    loop {
        let first = argv.first().map(String::as_str);
        let next = match first {
            Some("time" | "nohup") => skip_dashes(1, &argv),
            Some("timeout") => {
                let Some(at) = timeout_duration(&argv, patterns) else {
                    return argv;
                };
                if !argv
                    .get(at)
                    .is_some_and(|word| !word.is_empty() && patterns.duration.is_match(word))
                {
                    return argv;
                }
                at + 1
            }
            Some("nice")
                if argv.get(1).is_some_and(|word| word == "-n")
                    && argv.get(2).is_some_and(|word| {
                        !word.is_empty() && patterns.niceness.is_match(word)
                    }) =>
            {
                skip_dashes(NICE_DASH_N_WORDS, &argv)
            }
            _ => return argv,
        };
        argv.drain(..next.min(argv.len()));
    }
}

/// `command -v` / `-V` 一类的查询选项：不执行命令。
fn query_option(word: &str) -> bool {
    word.len() > 1
        && word.starts_with('-')
        && word[1..].bytes().all(|b| matches!(b, b'p' | b'v' | b'V'))
        && word[1..].bytes().any(|b| matches!(b, b'v' | b'V'))
}

/// 去掉 argv 开头的前缀赋值与包装命令，返回真正执行的命令（§4.5.6）。
pub(crate) fn strip_wrappers(argv: &[String]) -> Vec<String> {
    let Some(patterns) = patterns() else {
        return argv.to_vec();
    };
    let mut words = argv.to_vec();
    loop {
        let skip = words
            .iter()
            .take_while(|word| patterns.assignment.is_match(word))
            .count();
        words.drain(..skip);
        words = strip_runners(words, patterns);
        let Some(name) = words.first().cloned() else {
            return words;
        };
        let Some((_, valued)) = WITH_VALUES.iter().find(|(wrapper, _)| *wrapper == name) else {
            return words;
        };
        let commands = command_options(&name);
        let mut index = 1;
        let mut skipped_positional = false;
        let mut inline: Option<String> = None;
        while index < words.len() {
            let word = words[index].as_str();
            if word == "--" {
                index += 1;
                if !skipped_positional
                    && index + 1 < words.len()
                    && positional(&name, &words[index]) == Some(true)
                {
                    skipped_positional = true;
                    index += 1;
                    continue;
                }
                break;
            }
            if !commands.is_empty() {
                if commands.contains(&word) && index + 1 < words.len() {
                    let value = words[index + 1].trim();
                    if !value.is_empty() {
                        inline = Some(value.to_owned());
                        break;
                    }
                    index += OPTION_WITH_VALUE_WORDS;
                    continue;
                }
                if let Some(eq) = word.find('=').filter(|eq| *eq > 0)
                    && commands.contains(&&word[..eq])
                {
                    let value = word[eq + 1..].trim();
                    if !value.is_empty() {
                        inline = Some(value.to_owned());
                        break;
                    }
                    index += 1;
                    continue;
                }
                if word.len() > SHORT_OPTION_LEN
                    && word.as_bytes()[1] != b'-'
                    && word.is_char_boundary(SHORT_OPTION_LEN)
                    && commands.contains(&&word[..SHORT_OPTION_LEN])
                {
                    let value = word[SHORT_OPTION_LEN..].trim();
                    if !value.is_empty() {
                        inline = Some(value.to_owned());
                        break;
                    }
                    index += 1;
                    continue;
                }
            }
            if word.starts_with('-') && (word != "-" || positional(&name, word).is_none()) {
                if name == "command" && query_option(word) {
                    return words;
                }
                index += if valued.contains(&word) && index + 1 < words.len() {
                    OPTION_WITH_VALUE_WORDS
                } else {
                    1
                };
                continue;
            }
            if name == "env" && patterns.assignment.is_match(word) {
                index += 1;
                continue;
            }
            if !skipped_positional
                && positional(&name, word) == Some(true)
                && index + 1 < words.len()
            {
                skipped_positional = true;
                index += 1;
                continue;
            }
            break;
        }
        if let Some(text) = inline {
            let split: Vec<String> = text.split_whitespace().map(str::to_owned).collect();
            if split.is_empty() {
                return argv.to_vec();
            }
            words = split;
            continue;
        }
        if index >= words.len() {
            return words;
        }
        words.drain(..index);
    }
}

#[cfg(test)]
#[path = "shell_argv_tests.rs"]
mod tests;
