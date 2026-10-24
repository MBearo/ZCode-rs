//! cd 之后的路径（shell-permissions.md §4.5.7，工作线 F85）。
//!
//! 一组子命令（一截管道，或没有管道时整条命令）里有 `cd` 时，后面各段的相对路径不一定按会话的当前目录解析。照对照实现：
//! 1. 原文正好是 `cd <会话当前目录>` 的段去掉，不算 cd；
//! 2. 首个 cd 指向工作目录里（`./x`、绝对路径）而且只用 `&&` 连接时，后面各段按它解析，这一组不算“有 cd”（快路）；
//! 3. 否则按顺序走一遍：cd 更新“可能的当前目录”集合，其余段在每个目录下查路径 deny 规则，命中就拒绝（文字是路径那一句）；
//!    有不能解析的 cd 之后，读的相对路径再按每条 Read deny 规则的目录拼出候选去查。
//!
//! 修复（工作线 F85）：以前后面各段总按会话的当前目录解析，`Read(//etc/**)` 下 `cd /etc && cat hosts` 只问 cd，guarded 与
//! yolo 下读到 `/etc/hosts`。

use rust_cli_kernel_api::env_files::PathStyle;
use rust_cli_kernel_api::env_path::git_bash_spelling;
use rust_cli_kernel_api::permission::RuleBehavior;

use crate::bash_paths;
use crate::check::{Check, Inputs};
use crate::files::{self, Access, matched_path_rule};
use crate::parse::{Invocation, analyze};
use crate::paths::{PathContext, anchor};
use crate::removal::{bare_home, strip_runners};
use crate::shell_rules::strip_safe_wrappers;
use crate::texts;

/// 一组子命令的 cd 结论。
pub(crate) struct Group {
    /// 去掉的段（`cd <会话当前目录>`）。
    pub dropped: Vec<bool>,
    /// 这一组“有 cd”（没走快路）：cd 加写入、cd 之后读相对路径的询问按它。
    pub has_cd: bool,
    /// 快路：除第一段（cd 自己）以外各段的当前目录。
    pub moved: Option<String>,
}

/// 去掉安全的包装后第一个词是 cd / pushd / popd / chdir。
pub(crate) fn is_cd(text: &str, invocation: Option<&Invocation>) -> bool {
    invocation.is_some()
        && analyze(&strip_safe_wrappers(text))
            .commands
            .first()
            .is_some_and(|first| matches!(first.name.as_str(), "cd" | "pushd" | "popd" | "chdir"))
}

/// 目录的写法里没有空白与 shell 的特殊字符。
fn plain_directory(directory: &str) -> bool {
    !directory.chars().any(|c| {
        c.is_whitespace()
            || matches!(
                c,
                '\\' | '$'
                    | '`'
                    | '\''
                    | '"'
                    | '*'
                    | '?'
                    | '['
                    | ']'
                    | '{'
                    | '}'
                    | '<'
                    | '>'
                    | '|'
                    | '&'
                    | ';'
                    | '('
                    | ')'
            )
    })
}

/// 只有 cd 与一个目标：没有前缀赋值与重定向，目标不是动态的、没有 glob。
fn bare_cd(invocation: &Invocation) -> Option<&str> {
    let [name, target] = invocation.argv.as_slice() else {
        return None;
    };
    (name == "cd"
        && invocation.env_assignments.is_empty()
        && invocation.redirects.is_empty()
        && !invocation.has_dynamic_words
        && !target.contains(['*', '?', '[', ']']))
    .then_some(target.as_str())
}

/// `cd <会话当前目录>`（§4.5.7 第 1 步）。
fn to_current(text: &str, invocation: Option<&Invocation>, paths: &PathContext) -> bool {
    let Some(invocation) = invocation else {
        return false;
    };
    let mut spellings = vec![paths.cwd.clone()];
    if paths.style == PathStyle::Windows {
        spellings.push(git_bash_spelling(&paths.cwd));
    }
    bare_cd(invocation).is_some()
        && spellings
            .iter()
            .any(|cwd| plain_directory(cwd) && text == format!("cd {cwd}"))
}

/// 只用 `&&` 连接（没有 `||`、`;`、换行、单独的 `&`）。
pub(crate) fn only_and(text: &str) -> bool {
    !text.contains("||")
        && !text.contains(';')
        && !text.contains('\n')
        && !text.replace("&&", "").contains('&')
}

/// 快路的目标（§4.5.7）：`cd ./x`、`cd .`、`cd <绝对路径>`，读它放行而且在工作目录里。
fn leading_target(invocation: &Invocation, inputs: &Inputs<'_>) -> Option<String> {
    let paths = inputs.paths;
    let target = bare_cd(invocation)?;
    // Windows 上对照实现另有盘符写法的检查，这里不走快路（按集合查，D25）。
    if target.starts_with('-') || paths.style == PathStyle::Windows {
        return None;
    }
    let absolute = paths.style.is_absolute(target);
    let explicit = absolute
        || target.starts_with("./")
        || target.starts_with("../")
        || target == "."
        || target == "..";
    if !explicit || (!absolute && target.split(['/', '\\']).any(|part| part == "..")) {
        return None;
    }
    let resolved = paths.resolve(target);
    (files::check(Access::Read, Some(&resolved), inputs).is_allow()
        && paths.in_working_dirs(&resolved))
    .then_some(resolved)
}

/// 一组子命令（原文与解析结果，按顺序）的 cd 结论；拒绝时 Err（文字是路径那一句）。`text` 是这一组的原文。
pub(crate) fn plan(
    text: &str,
    parts: &[(&str, Option<&Invocation>)],
    inputs: &Inputs<'_>,
) -> Result<Group, Check> {
    let dropped: Vec<bool> = parts
        .iter()
        .map(|(text, invocation)| to_current(text, *invocation, inputs.paths))
        .collect();
    let kept: Vec<(&str, Option<&Invocation>)> = parts
        .iter()
        .zip(&dropped)
        .filter(|(_, dropped)| !**dropped)
        .map(|(part, _)| *part)
        .collect();
    let cds = kept
        .iter()
        .filter(|(text, invocation)| is_cd(text, *invocation))
        .count();
    let unmoved = |has_cd| Group {
        dropped: dropped.clone(),
        has_cd,
        moved: None,
    };
    if cds == 0 {
        return Ok(unmoved(false));
    }
    if cds == 1
        && kept.len() > 1
        && !dropped.contains(&true)
        && is_cd(kept[0].0, kept[0].1)
        && only_and(text)
        && let Some(target) = kept[0].1.and_then(|first| leading_target(first, inputs))
    {
        return Ok(Group {
            dropped,
            has_cd: false,
            moved: Some(target),
        });
    }
    // 两个以上 cd 时对照实现之后还询问 multi-cd，本工程不问（D25）。
    match track(parts, inputs) {
        Some(denied) => Err(denied),
        None => Ok(unmoved(true)),
    }
}

/// cd 的目标与它的原文（`$HOME` 换成 home）：(值, 有没有别的展开)。
fn cd_arguments(
    invocation: &Invocation,
    argv: &[String],
    home: Option<&str>,
) -> Vec<(String, bool)> {
    let offset = invocation.words.len().saturating_sub(argv.len());
    argv.iter()
        .enumerate()
        .skip(1)
        .map(|(index, value)| {
            let raw = invocation
                .words
                .get(offset + index)
                .map_or(value.as_str(), |word| word.text.as_str());
            match (bare_home(raw), home) {
                (Some(rest), Some(home)) => (format!("{home}{rest}"), false),
                _ => (value.clone(), value.contains('$')),
            }
        })
        .collect()
}

/// 可能的当前目录（§4.5.7）：按顺序走一遍，其余段在每个目录下查路径 deny 规则。
fn track(parts: &[(&str, Option<&Invocation>)], inputs: &Inputs<'_>) -> Option<Check> {
    let paths = inputs.paths;
    let home = paths.home.as_deref();
    let resolve = |base: &str, target: &str| paths.style.resolve(base, target, home);
    let origin = paths.cwd.clone();
    let mut set: Vec<String> = vec![origin.clone()];
    fn add(set: &mut Vec<String>, dir: String) {
        if !set.contains(&dir) {
            set.push(dir);
        }
    }
    let mut current = Some(origin.clone());
    let mut previous: Option<Option<String>> = None;
    let mut stack: Vec<Option<String>> = Vec::new();
    let mut unresolved = false;
    // 目标里有 `$HOME` 以外的展开：对照实现整条 too-complex，不会走到这里；不做下面按规则目录的拒绝（不比它严）。
    let mut expansion = false;
    for (_, invocation) in parts {
        let Some(invocation) = invocation else {
            continue;
        };
        let argv = strip_runners(&invocation.argv);
        let name = argv
            .first()
            .map_or("", |first| crate::removal::basename(first));
        if matches!(name, "cd" | "chdir" | "pushd" | "popd") {
            let arguments = cd_arguments(invocation, argv, home);
            expansion |= arguments.iter().any(|(_, dynamic)| *dynamic)
                || !invocation.static_assignments && !invocation.env_assignments.is_empty();
            let positional: Vec<&str> = arguments
                .iter()
                .map(|(value, _)| value.as_str())
                .filter(|value| *value != "--" && (*value == "-" || !value.starts_with('-')))
                .collect();
            let target = match (positional.as_slice(), name) {
                ([one], _) => Some(*one),
                ([], "cd" | "chdir") => Some("~"),
                _ => None,
            };
            let flags: Vec<&str> = arguments
                .iter()
                .map(|(value, _)| value.as_str())
                .filter(|value| {
                    *value != "--"
                        && *value != "-"
                        && value.starts_with('-')
                        && !(value.len() > 1 && value[1..].bytes().all(|b| b"PLe@".contains(&b)))
                })
                .collect();
            if matches!(name, "pushd" | "popd") && flags == ["-n"] {
                continue;
            }
            let dynamic_env = invocation.env_assignments.iter().any(|assignment| {
                assignment
                    .value
                    .as_deref()
                    .is_some_and(|value| value.contains(['$', '`']))
            });
            let resolvable = name != "popd"
                && flags.is_empty()
                && target.is_some_and(|target| !unresolvable_target(target))
                && !dynamic_env;
            let back = if !flags.is_empty() {
                None
            } else if name == "popd" {
                if positional.is_empty() {
                    stack.pop()
                } else {
                    None
                }
            } else if target == Some("-") {
                previous.clone()
            } else {
                None
            };
            let moved = back.is_some() || resolvable;
            if let Some(dir) = back {
                previous = Some(std::mem::replace(&mut current, dir));
            } else if let (true, Some(target)) = (resolvable, target) {
                if name == "pushd" {
                    stack.push(current.clone());
                }
                previous = Some(current.clone());
                add(&mut set, resolve(&origin, target));
                if let Some(base) = &current {
                    let next = resolve(base, target);
                    add(&mut set, next.clone());
                    current = Some(next);
                }
            } else {
                if name == "pushd" {
                    stack.push(current.clone());
                }
                unresolved = true;
                previous = Some(current.take());
            }
            if current.is_none() && moved {
                unresolved = true;
            }
            continue;
        }
        for dir in &set {
            let context = PathContext {
                cwd: dir.clone(),
                ..paths.clone()
            };
            let verdict = bash_paths::check(invocation, false, &inputs.with_paths(&context));
            if matches!(verdict.check, Check::Deny { .. }) {
                return Some(verdict.check);
            }
        }
        if unresolved && !expansion {
            let command = argv.first().map_or("", String::as_str);
            if let Some(denied) = rule_directories(command, invocation, inputs) {
                return Some(denied);
            }
        }
    }
    None
}

/// 目标不能静态解析：`-`、`+N` / `-N`、glob、展开、`~user`。
fn unresolvable_target(target: &str) -> bool {
    let other_user = target.starts_with('~') && target != "~" && !target.starts_with("~/");
    target == "-"
        || signed_number(target)
        || target.contains(['*', '?', '[', ']', '$', '`'])
        || other_user
}

/// `+N` / `-N`（目录栈的位置）。
fn signed_number(target: &str) -> bool {
    target
        .strip_prefix(['+', '-'])
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

/// 不能解析的 cd 之后：这一段读的相对路径接在每条 Read deny 规则的根与模式的各级前缀下面去查（§4.5.7）。
fn rule_directories(command: &str, invocation: &Invocation, inputs: &Inputs<'_>) -> Option<Check> {
    let paths = inputs.paths;
    let home = paths.home.as_deref();
    let denied = |path: &str, shown: &str| {
        matched_path_rule(Access::Read, path, RuleBehavior::Deny, inputs)
            .map(|rule| Check::deny(texts::unresolved_cd_denied(command, shown, rule)))
    };
    for shown in bash_paths::read_paths(invocation) {
        if paths.style.is_absolute(&shown) || shown.starts_with('~') {
            if let Some(found) = denied(&paths.resolve(&shown), &shown) {
                return Some(found);
            }
            continue;
        }
        let mut parts: Vec<&str> = shown
            .split(['/', '\\'])
            .filter(|part| !part.is_empty() && *part != ".")
            .collect();
        while parts.first() == Some(&"..") {
            parts.remove(0);
        }
        if parts.is_empty() {
            continue;
        }
        let tail = parts.join("/");
        for (pattern, _) in inputs
            .rules
            .contents(Access::Read.rule_tool(), RuleBehavior::Deny)
        {
            let (base, glob) = anchor(pattern, inputs.root, paths);
            let levels: Vec<&str> = glob.split('/').filter(|part| !part.is_empty()).collect();
            for depth in 0..levels.len().max(1) {
                let mut relative: Vec<&str> = levels[..depth.min(levels.len())].to_vec();
                relative.push(&tail);
                let candidate = paths.style.resolve(&base, &relative.join("/"), home);
                if let Some(found) = denied(&candidate, &shown) {
                    return Some(found);
                }
            }
        }
    }
    None
}
