//! `ss`, `gh` and `docker` callbacks (`bash-readonly-policy-callbacks.ts`).
use super::js;

pub(crate) fn ss(args: &[String]) -> bool {
    let mut positional: Vec<&str> = vec![];
    let mut after_double_dash = false;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        index += 1;
        if !after_double_dash && arg == "--" {
            after_double_dash = true;
            continue;
        }
        if !after_double_dash && arg.starts_with('-') {
            if matches!(arg, "-f" | "--family" | "-A" | "--query" | "--socket") {
                index += 1;
            }
            continue;
        }
        positional.push(arg);
    }
    let joined = positional.join(" ");
    // `/[\s()=!<>&|,]+/` 切分并丢弃空串。
    let separator = |c: char| js::is_space(c) || "()=!<>&|,".contains(c);
    let mut skip = false;
    for token in joined.split(separator).filter(|t| !t.is_empty()) {
        if skip {
            skip = false;
            continue;
        }
        let keyword = matches!(
            token,
            "dst"
                | "src"
                | "dport"
                | "sport"
                | "and"
                | "or"
                | "not"
                | "eq"
                | "ne"
                | "ge"
                | "le"
                | "gt"
                | "lt"
                | "autobound"
                | "state"
                | "exclude"
                | "dev"
                | "fwmark"
                | "cgroup"
        );
        if keyword {
            skip = matches!(
                token,
                "state" | "exclude" | "dport" | "sport" | "dev" | "fwmark" | "cgroup"
            );
            continue;
        }
        let letter = token
            .bytes()
            .any(|b| matches!(b, b'g'..=b'z' | b'G'..=b'Z'));
        let hex_letter = token
            .bytes()
            .any(|b| matches!(b, b'a'..=b'f' | b'A'..=b'F'));
        if letter || (hex_letter && (token.contains('.') || !token.contains(':'))) {
            return true;
        }
    }
    false
}

pub(crate) fn gh(args: &[String]) -> bool {
    for arg in args {
        if arg.is_empty() {
            continue;
        }
        let mut value = arg.as_str();
        if arg.starts_with('-') {
            let Some((_, inline)) = arg.split_once('=') else {
                continue;
            };
            if inline.is_empty() {
                continue;
            }
            value = inline;
        }
        if value.contains("://") || value.contains('@') {
            return true;
        }
        if value.matches('/').count() >= 2 {
            return true;
        }
    }
    false
}

const DOCKER_DANGEROUS: [&str; 8] = [
    "-H",
    "-c",
    "--config",
    "--context",
    "--host",
    "--tlscacert",
    "--tlscert",
    "--tlskey",
];

pub(crate) fn docker(args: &[String]) -> bool {
    has_dangerous_docker_option(args)
}

pub(crate) fn has_dangerous_docker_option(args: &[String]) -> bool {
    args.iter().any(|arg| {
        let flag_hit = DOCKER_DANGEROUS.iter().any(|flag| {
            arg == flag
                || arg.strip_prefix(flag).is_some_and(|r| r.starts_with('='))
                || (flag.len() == 2 && js::utf16_len(arg) > 2 && arg.starts_with(flag))
        });
        // `/^-([A-Za-z]+)/` 捕获的短参数簇长度 ≥ 2 时逐字母检查。
        let cluster = arg
            .strip_prefix('-')
            .map(|rest| &rest[..rest.bytes().take_while(u8::is_ascii_alphabetic).count()])
            .unwrap_or("");
        flag_hit || (cluster.len() >= 2 && cluster.contains(['H', 'c']))
    })
}
