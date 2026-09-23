//! Git subcommand callbacks (`bash-readonly-policy-git-callbacks.ts`).

pub(crate) fn by_name(name: &str) -> Option<super::tables::Callback> {
    Some(match name {
        "gitRevisionFormatCommandIsDangerous" => revision_format,
        "gitReflogCommandIsDangerous" => reflog,
        "gitLsRemoteCommandIsDangerous" => ls_remote,
        "gitRemoteShowCommandIsDangerous" => remote_show,
        "gitTagCommandIsDangerous" => tag,
        "gitBranchCommandIsDangerous" => branch,
        // `git remote` 的内联回调：只允许 -v / --verbose。
        "inline:git remote" => |args| args.iter().any(|a| a != "-v" && a != "--verbose"),
        _ => return None,
    })
}

/// `%G…` and `%(signature)` make git verify signatures (runs gpg).
fn revision_format(args: &[String]) -> bool {
    for (index, arg) in args.iter().enumerate() {
        let value = match arg.split_once('=') {
            Some((_, value)) => Some(value),
            None => args.get(index + 1).map(String::as_str),
        };
        let format = arg == "--format"
            || arg == "--pretty"
            || arg.starts_with("--format=")
            || arg.starts_with("--pretty=");
        if format
            && let Some(value) = value.filter(|v| !v.is_empty())
            && signature_placeholder(value)
        {
            return true;
        }
    }
    false
}

/// `/%[-+ ]?G|%\(\*?signature/`.
fn signature_placeholder(value: &str) -> bool {
    value.match_indices('%').any(|(at, _)| {
        let rest = &value[at + 1..];
        let rest_g = rest.strip_prefix(['-', '+', ' ']).unwrap_or(rest);
        rest.starts_with('G')
            || rest_g.starts_with('G')
            || rest
                .strip_prefix('(')
                .is_some_and(|r| r.strip_prefix('*').unwrap_or(r).starts_with("signature"))
    })
}

fn reflog(args: &[String]) -> bool {
    let first = args.iter().find(|a| !a.is_empty() && !a.starts_with('-'));
    if first.is_some_and(|f| f != "show" && f != "list") {
        return true;
    }
    args.iter().any(|a| {
        matches!(
            a.as_str(),
            "expire" | "delete" | "exists" | "drop" | "write"
        )
    })
}

fn ls_remote(args: &[String]) -> bool {
    let mut after_double_dash = false;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        index += 1;
        if !after_double_dash && arg == "--" {
            after_double_dash = true;
            continue;
        }
        if !after_double_dash && (arg.starts_with('-') || arg.is_empty()) {
            if arg == "--sort" {
                index += 1;
            }
            continue;
        }
        return true;
    }
    false
}

fn remote_show(args: &[String]) -> bool {
    let split = args.iter().position(|a| a == "--");
    let options = &args[..split.unwrap_or(args.len())];
    let mut positional: Vec<&String> = split.map_or(vec![], |i| args[i + 1..].iter().collect());
    positional.extend(options.iter().filter(|a| *a != "-n"));
    if !options.iter().any(|a| a == "-n") || positional.len() != 1 {
        return true;
    }
    // `/^[a-zA-Z0-9_][a-zA-Z0-9_-]*$/`。
    let name = positional[0].as_bytes();
    !(name
        .first()
        .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
        && name
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')))
}

fn tag(args: &[String]) -> bool {
    list_like(
        args,
        &[
            "--contains",
            "--no-contains",
            "--merged",
            "--no-merged",
            "--points-at",
            "--sort",
            "--format",
            "-n",
        ],
    )
}

fn branch(args: &[String]) -> bool {
    list_like(
        args,
        &["--contains", "--no-contains", "--points-at", "--sort"],
    )
}

fn list_like(args: &[String], value_flags: &[&str]) -> bool {
    let mut list = false;
    let mut after_double_dash = false;
    let mut previous = "";
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        index += 1;
        if arg.is_empty() {
            continue;
        }
        if arg == "--" && !after_double_dash {
            after_double_dash = true;
            previous = "";
            continue;
        }
        if !after_double_dash && arg.starts_with('-') {
            if arg == "--list"
                || arg == "-l"
                || (!arg[1..].starts_with('-') && arg[1..].contains('l'))
            {
                list = true;
            }
            previous = arg.split_once('=').map_or(arg, |(flag, _)| flag);
            if !arg.contains('=') && value_flags.contains(&previous) {
                index += 1;
            }
            continue;
        }
        if !list && previous != "--merged" && previous != "--no-merged" {
            return true;
        }
    }
    false
}
