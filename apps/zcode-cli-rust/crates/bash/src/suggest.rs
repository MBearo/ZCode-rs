//! Stable "always allow" prefixes (`bash-command-permission-policy.ts`).
use super::{
    js,
    registry::{self, ARG_IS_COMMAND, ARG_IS_MODULE, Node},
};
use zcode_cli_bash_parse::Invocation;

const HIGH_RISK: [&str; 16] = [
    "bash",
    "chgrp",
    "chmod",
    "chown",
    "cmd",
    "dd",
    "fish",
    "mkfs",
    "mount",
    "powershell",
    "pwsh",
    "rm",
    "rmdir",
    "sh",
    "umount",
    "zsh",
];

/// `WRAPPER_OPTIONS_WITH_VALUES`: wrapper → options that take a value.
fn wrapper_value_options(name: &str) -> Option<&'static [&'static str]> {
    Some(match name {
        "command" | "nohup" => &[],
        "env" => &[
            "-C",
            "-S",
            "-u",
            "--argv0",
            "--chdir",
            "--split-string",
            "--unset",
        ],
        "sudo" => &[
            "-C",
            "-D",
            "-R",
            "-T",
            "-a",
            "-c",
            "-g",
            "-h",
            "-p",
            "-r",
            "-t",
            "-u",
            "--askpass",
            "--chdir",
            "--chroot",
            "--close-from",
            "--group",
            "--host",
            "--prompt",
            "--role",
            "--type",
            "--user",
        ],
        "time" => &["-f", "-o", "--format", "--output"],
        _ => return None,
    })
}

/// Own properties of `Object.prototype`. Node looks family depths up in a
/// plain object, so these first words yield a function or object instead of
/// `undefined` and suppress the `*` depth (kept from Node).
const PROTOTYPE_KEYS: [&str; 12] = [
    "constructor",
    "__defineGetter__",
    "__defineSetter__",
    "hasOwnProperty",
    "__lookupGetter__",
    "__lookupSetter__",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "toString",
    "valueOf",
    "__proto__",
    "toLocaleString",
];

fn family_depth(executable: &str, first: Option<&str>) -> Option<usize> {
    let (named, any): (&[(&str, usize)], Option<usize>) = match executable {
        "aws" | "az" => (&[], Some(2)),
        "docker" => (&[("compose", 2)], None),
        "gcloud" => (&[], Some(3)),
        "kubectl" => (&[("config", 2)], None),
        _ => return None,
    };
    let first = first.unwrap_or("");
    if PROTOTYPE_KEYS.contains(&first) {
        return None;
    }
    named
        .iter()
        .find(|(name, _)| *name == first)
        .map(|(_, depth)| *depth)
        .or(any)
}

/// `^[A-Za-z_][A-Za-z0-9_]*=[A-Za-z0-9_./:@,+-]*$`.
pub(crate) fn is_static_assignment(token: &str) -> bool {
    let Some((name, value)) = token.split_once('=') else {
        return false;
    };
    let name = name.as_bytes();
    name.first()
        .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        && name.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'_')
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_./:@,+-".contains(&b))
}

pub(crate) fn static_assignments(part: &Invocation) -> Option<Vec<String>> {
    part.env_assignments
        .iter()
        .map(|a| {
            let (name, value) = (a.name.as_deref()?, a.value.as_deref()?);
            let token = format!("{name}={value}");
            (!name.is_empty() && is_static_assignment(&token)).then_some(token)
        })
        .collect()
}

pub(crate) fn looks_like_path_or_url(token: &str) -> bool {
    let b = token.as_bytes();
    token.contains("://")
        || token.starts_with("./")
        || token.starts_with("../")
        || token.starts_with('/')
        || token.starts_with('~')
        || (b.len() >= 3
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && matches!(b[2], b'\\' | b'/'))
}

fn stable_action(token: Option<&String>) -> bool {
    token.is_some_and(|t| {
        !t.is_empty() && !t.starts_with('-') && !looks_like_path_or_url(t) && !js::has_space(t)
    })
}

fn serialize(tokens: &[String]) -> Option<String> {
    (tokens.len() >= 2 && tokens.iter().all(|t| !t.is_empty() && !js::has_space(t)))
        .then(|| tokens.join(" "))
}

fn basename(token: &str) -> String {
    let normalized = token.replace('\\', "/");
    normalized[normalized.rfind('/').map_or(0, |i| i + 1)..].to_lowercase()
}

struct Unwrapped {
    executable: String,
    next: usize,
    prefix: Vec<String>,
}

/// Node `unwrapCommand`: at most two of `command`, `env`, `nohup`, `sudo`, `time`.
fn unwrap(argv: &[String]) -> Option<Unwrapped> {
    let mut prefix = vec![];
    let mut index = 0;
    let mut depth = 0;
    while index < argv.len() {
        let token = &argv[index];
        let name = basename(token);
        let Some(value_options) = wrapper_value_options(&name) else {
            return Some(Unwrapped {
                executable: token.clone(),
                next: index + 1,
                prefix,
            });
        };
        depth += 1;
        if depth > 2 {
            return None;
        }
        prefix.push(token.clone());
        index += 1;
        while let Some(word) = argv.get(index) {
            if name == "env" && is_static_assignment(word) {
                prefix.push(word.clone());
                index += 1;
                continue;
            }
            let option = word.split_once('=').map_or(word.as_str(), |(o, _)| o);
            if value_options.contains(&option) {
                index += if word.contains('=') { 1 } else { 2 };
                continue;
            }
            if word.starts_with('-') {
                index += 1;
                continue;
            }
            break;
        }
    }
    None
}

fn depth_override(executable: &str, args: &[String]) -> Option<Vec<String>> {
    let first = args.first().map(String::as_str);
    if matches!(executable, "python" | "python3" | "py")
        && first == Some("-m")
        && stable_action(args.get(1))
    {
        return Some(args[..2].to_vec());
    }
    let script = match executable {
        "bun" | "pnpm" | "yarn" => first == Some("run"),
        "deno" => first == Some("task"),
        "npm" => matches!(first, Some("run" | "run-script")),
        _ => false,
    };
    if script && stable_action(args.get(1)) {
        return Some(args[..2].to_vec());
    }
    if matches!(executable, "just" | "make") && stable_action(args.first()) {
        return Some(args[..1].to_vec());
    }
    let depth = family_depth(executable, first)?;
    (depth > 0 && args.len() >= depth && args[..depth].iter().all(|a| stable_action(Some(a))))
        .then(|| args[..depth].to_vec())
}

fn skip_option(node: &Node, args: &[String], index: usize) -> Option<usize> {
    let token = &args[index];
    if !token.starts_with('-') || token == "-" {
        return None;
    }
    if token == "--" {
        return Some(index + 1);
    }
    let name = token.split_once('=').map_or(token.as_str(), |(n, _)| n);
    let option = node.1.iter().find(|o| o.0.iter().any(|n| n == name))?;
    Some(
        index
            + if option.1 == 1 && !token.contains('=') {
                2
            } else {
                1
            },
    )
}

/// Node `resolveStableCommandPrefix`.
pub(crate) fn stable_prefix(part: &Invocation) -> Option<String> {
    let assignments = static_assignments(part)?;
    if part.argv.is_empty() {
        return None;
    }
    let unwrapped = unwrap(&part.argv)?;
    let executable = basename(&unwrapped.executable);
    if HIGH_RISK.contains(&executable.as_str()) {
        return None;
    }
    let mut prefix = assignments;
    prefix.extend(unwrapped.prefix);
    prefix.push(unwrapped.executable);
    let remaining = &part.argv[unwrapped.next.min(part.argv.len())..];
    if let Some(action) = depth_override(&executable, remaining) {
        prefix.extend(action);
        return serialize(&prefix);
    }
    let mut node = registry::command(&executable)?;
    let mut start = 0;
    while start < remaining.len() {
        match skip_option(node, remaining, start) {
            Some(next) => start = next,
            None => break,
        }
    }
    if let Some(action) = depth_override(&executable, &remaining[start.min(remaining.len())..]) {
        prefix.extend(action);
        return serialize(&prefix);
    }
    let mut matched = false;
    let mut index = 0;
    while index < remaining.len() {
        if let Some(next) = skip_option(node, remaining, index) {
            index = next;
            continue;
        }
        let token = &remaining[index];
        if let Some(child) = node.3.iter().find(|c| c.0.iter().any(|n| n == token)) {
            prefix.push(token.clone());
            matched = true;
            node = child;
            index += 1;
            continue;
        }
        if node.2 & (ARG_IS_COMMAND | ARG_IS_MODULE) != 0 && !looks_like_path_or_url(token) {
            prefix.push(token.clone());
            matched = true;
        }
        break;
    }
    matched.then(|| serialize(&prefix)).flatten()
}
