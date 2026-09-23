//! Git argv handling (`bash-readonly-policy-argv-git.ts`) and the command-name
//! checks of `bash-git-runtime-safety.ts`.
use super::{argv::strip_wrappers, flags::argv_allowed, tables::tables};
use zcode_cli_bash_parse::Invocation;

/// `-c`/`-C` and friends anywhere in the argv, attached forms included.
pub(crate) fn dangerous_global_word(word: &str) -> bool {
    let attached = ["-c", "-C"].iter().any(|flag| {
        word.len() > flag.len()
            && word.starts_with(flag)
            && (*flag == "-C" || word.as_bytes()[flag.len()] != b'-')
    });
    attached
        || tables().git_dangerous.iter().any(|flag| {
            word == flag
                || word
                    .strip_prefix(flag.as_str())
                    .is_some_and(|r| r.starts_with('='))
        })
}

/// Node `normalizeGitArgv`: drops pager flags; any other global option or a
/// missing subcommand makes the command not read-only.
fn normalize(argv: &[String]) -> Option<Vec<&str>> {
    let tables = tables();
    let mut normalized = vec!["git"];
    let mut index = 1;
    while index < argv.len() {
        let word = argv[index].as_str();
        index += 1;
        if word.is_empty() || tables.git_no_value.contains(word) {
            continue;
        }
        if dangerous_global_word(word) {
            return None;
        }
        if tables.git_value.contains(word) {
            if argv.get(index).is_none_or(|v| v.is_empty()) {
                return None;
            }
            index += 1;
            continue;
        }
        if word.starts_with('-') {
            return None;
        }
        normalized.extend(argv[index - 1..].iter().map(String::as_str));
        return Some(normalized);
    }
    None
}

pub(crate) fn read_only(argv: &[String]) -> bool {
    let Some(normalized) = normalize(argv) else {
        return false;
    };
    let Some(policy) = tables().git.iter().find(|p| {
        p.words.len() <= normalized.len() && p.words.iter().zip(&normalized).all(|(w, n)| w == n)
    }) else {
        return false;
    };
    let owned: Vec<String> = normalized.iter().map(|s| (*s).to_owned()).collect();
    if policy
        .callback
        .is_some_and(|cb| cb(&owned[policy.words.len()..]))
    {
        return false;
    }
    argv_allowed(&owned, policy, "git", policy.words.len())
}

/// Node `normalizedSimpleCommandName(argv) ?? name`.
fn command_name(part: &Invocation) -> &str {
    strip_wrappers(&part.argv)
        .first()
        .map_or(part.name.as_str(), String::as_str)
}

/// Node `analysisContainsGitCommand`.
pub fn invokes_git(commands: &[Invocation]) -> bool {
    commands.iter().any(|c| command_name(c) == "git")
}

/// Node `analysisContainsGitAndDirectoryChange`: git may load hooks and config
/// from the directory a `cd` moves to.
pub(crate) fn git_with_directory_change(commands: &[Invocation]) -> bool {
    invokes_git(commands)
        && commands
            .iter()
            .any(|c| matches!(command_name(c), "cd" | "pushd" | "popd"))
}
