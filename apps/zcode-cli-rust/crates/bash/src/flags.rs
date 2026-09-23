//! Node `isArgvAllowedByPolicy` (`bash-readonly-policy-argv-flags.ts`).
use super::{
    callbacks::XARGS_TARGETS,
    js,
    tables::{Kind, Policy},
};
use std::collections::HashMap;

/// `OPTION_PATTERN` (`/^-[a-zA-Z0-9_-]/`); also implies length > 1.
pub(crate) fn option_like(word: &str) -> bool {
    let bytes = word.as_bytes();
    bytes.first() == Some(&b'-')
        && bytes
            .get(1)
            .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

pub(crate) fn argv_allowed(argv: &[String], policy: &Policy, command: &str, start: usize) -> bool {
    if argv.is_empty() {
        return false;
    }
    if policy.allow_any_args {
        return true;
    }
    if policy.command_only {
        return argv.len() == start;
    }
    match &policy.safe_flags {
        Some(flags) => scan(argv, policy, flags, command, start),
        None => false,
    }
}

fn scan(
    argv: &[String],
    policy: &Policy,
    flags: &HashMap<String, Kind>,
    command: &str,
    start: usize,
) -> bool {
    let mut index = start;
    while index < argv.len() {
        let word = argv[index].as_str();
        if word.is_empty() {
            index += 1;
            continue;
        }
        if command == "xargs" && (!word.starts_with('-') || word == "--") {
            let target = if word == "--" {
                argv.get(index + 1).map(String::as_str)
            } else {
                Some(word)
            };
            // xargs 之后的内容交给目标命令，Node 在这里直接停止扫描。
            return target.is_some_and(|t| !t.is_empty() && XARGS_TARGETS.contains(&t));
        }
        if word == "--" {
            if !policy.respects_double_dash {
                index += 1;
                continue;
            }
            break;
        }
        if matches!(command, "head" | "tail") && js::is_dash_digits(word) {
            index += 1;
            continue;
        }
        if policy.allow_compact_numeric_count_flag && js::is_dash_digits(word) {
            index += 1;
            continue;
        }
        if option_like(word) {
            let (flag, inline) = match word.split_once('=') {
                Some((flag, value)) => (flag, Some(value)),
                None => (word, None),
            };
            match flags.get(flag).copied() {
                None => {
                    if let Some((short, kind, value)) = short_attached(word, flags) {
                        if !option_like_value_allowed(value, command, short)
                            || !kind_matches(value, kind)
                        {
                            return false;
                        }
                    } else if !short_cluster_allowed(flag, flags) {
                        return false;
                    }
                    index += 1;
                }
                Some(Kind::None) => {
                    if inline.is_some() {
                        return false;
                    }
                    index += 1;
                }
                Some(Kind::OptionalString) => index += 1,
                Some(kind) => {
                    let Some(value) = inline.or_else(|| argv.get(index + 1).map(String::as_str))
                    else {
                        return false;
                    };
                    if kind == Kind::String
                        && inline.is_none()
                        && !option_like_value_allowed(value, command, flag)
                    {
                        return false;
                    }
                    if !kind_matches(value, kind) {
                        return false;
                    }
                    index += if inline.is_some() { 1 } else { 2 };
                }
            }
            continue;
        }
        index += 1;
    }
    true
}

/// `-n3`: a known short flag taking a value, with the value attached.
fn short_attached<'a>(
    word: &'a str,
    flags: &HashMap<String, Kind>,
) -> Option<(&'a str, Kind, &'a str)> {
    if !word.starts_with('-') || word.starts_with("--") || js::utf16_len(word) <= 2 {
        return None;
    }
    let split = word.char_indices().nth(2).map_or(word.len(), |(i, _)| i);
    let flag = &word[..split];
    let kind = flags.get(flag).copied()?;
    (kind != Kind::None).then_some((flag, kind, &word[split..]))
}

/// `-abc` where every `-a`, `-b`, `-c` takes no value.
fn short_cluster_allowed(flag: &str, flags: &HashMap<String, Kind>) -> bool {
    if !flag.starts_with('-') || flag.starts_with("--") || js::utf16_len(flag) <= 2 {
        return false;
    }
    flag[1..]
        .chars()
        .all(|c| flags.get(&format!("-{c}")) == Some(&Kind::None))
}

fn option_like_value_allowed(value: &str, command: &str, flag: &str) -> bool {
    if !option_like(value) {
        return true;
    }
    command == "git"
        && flag == "--sort"
        && value.as_bytes().get(1).is_some_and(u8::is_ascii_alphabetic)
}

fn kind_matches(value: &str, kind: Kind) -> bool {
    match kind {
        Kind::None => false,
        Kind::Number => js::is_digits(value),
        Kind::OptionalString | Kind::String => true,
        Kind::Char => js::utf16_len(value) == 1,
        Kind::Braces => value == "{}",
        Kind::Eof => value == "EOF",
    }
}
