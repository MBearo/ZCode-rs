//! Bash 的权限检查（shell-permissions.md §4.5）。
//!
//! 整条命令先比精确规则；再按子命令（`&&`、`||`、`;`、`|`，加上从替换里拆出的命令，§4.5.2，与复合结构里的命令，
//! §4.5.4）逐个检查：deny 前缀 → ask 前缀 → 路径（含危险删除，§4.5.3）→ 精确 / 前缀 allow → acceptEdits 下的文件命令 →
//! 只读命令放行 → “需要批准”。规则比较的候选见 §4.5.6；有 cd 时每一段的当前目录按 §4.5.7（cd 之后命中路径 deny 规则
//! 时整条拒绝，文字是路径那一句）。任何一个拒绝就拒绝（文字是整条命令，有管道时是那一段，§4.5.5）；危险删除与“cd 加
//! 写入”按那个子命令询问；有子 shell 时询问 shell operators；全部放行才放行（顶层有 `&` 时改成询问一次，guarded 不问）；
//! 只有一个不放行且它是询问时按它询问；其余列出需要批准的部分。

use rust_cli_kernel_api::permission::{PermissionMode, RuleBehavior};

use crate::bash_compound::{self, BACKGROUND, SHELL_OPERATORS, Split};
use crate::bash_subst;
use crate::bash_suggest;
use crate::check::{AskKind, Check, Inputs};
use crate::parse::{Analysis, Invocation, analyze};
use crate::paths::PathContext;
use crate::readonly::is_read_only_on;
use crate::rules::Found;
use crate::shell_rules::{self, Mode, Subject};
use crate::texts::{self, Reason};
use crate::{bash_cd, bash_inline, bash_paths, removal};

const TOOL: &str = "Bash";
/// acceptEdits 下自动放行的文件命令。
const ACCEPT_EDITS: &[&str] = &["mkdir", "touch", "rm", "rmdir", "mv", "cp", "sed"];
/// 一条命令命中的第一条某种行为的 Bash 规则（候选按行为算，§4.5.6）。
fn found_rule<'a>(
    inputs: &Inputs<'a>,
    behavior: RuleBehavior,
    subject: Subject<'_>,
    mode: Mode,
) -> Option<Found<'a>> {
    let candidates = shell_rules::candidates(subject, mode, behavior);
    inputs
        .rules
        .contents(TOOL, behavior)
        .into_iter()
        .find(|(content, _)| shell_rules::matches(content, &candidates, mode, behavior))
        .map(|(_, found)| found)
}

/// 命中的 ask 规则换成询问。
fn ask_rule(inputs: &Inputs<'_>, subject: Subject<'_>, mode: Mode) -> Option<Check> {
    let found = found_rule(inputs, RuleBehavior::Ask, subject, mode)?;
    Some(Check::Ask {
        message: texts::request_message(TOOL, Some(&Reason::Rule(found.text(), found.source))),
        kind: AskKind::Rule,
        suggestions: Vec::new(),
    })
}

fn matches_any(
    inputs: &Inputs<'_>,
    behavior: RuleBehavior,
    subject: Subject<'_>,
    mode: Mode,
) -> bool {
    found_rule(inputs, behavior, subject, mode).is_some()
}

/// 一段的候选来源：原文、去掉重定向的原文、argv；有解析树时看有没有前缀赋值（§4.5.6）。
fn subject<'a>(text: &'a str, invocation: Option<&'a Invocation>) -> Subject<'a> {
    Subject {
        text,
        plain: invocation.map(|invocation| invocation.plain_text.as_str()),
        argv: invocation.map(|invocation| invocation.argv.as_slice()),
        assigned: invocation.is_some_and(|invocation| invocation.static_assignments),
    }
}

/// 没有解析树时的候选来源：argv 取第一条命令的，不看前缀赋值（§4.5.6）。
fn loose_subject<'a>(text: &'a str, first: Option<&'a Invocation>) -> Subject<'a> {
    Subject {
        text,
        plain: None,
        argv: first.map(|invocation| invocation.argv.as_slice()),
        assigned: false,
    }
}

/// 一个子命令的结论；`cd_with_write` 标出“复合命令里有 cd 又有写入”的询问，`redirect` 标出拒绝来自输出重定向的目标。
struct Sub {
    check: Check,
    cd_with_write: bool,
    redirect: bool,
}

impl From<Check> for Sub {
    fn from(check: Check) -> Self {
        Self {
            check,
            cd_with_write: false,
            redirect: false,
        }
    }
}

/// 一个子命令的检查。
fn check_subcommand(
    text: &str,
    invocation: Option<&Invocation>,
    compound_has_cd: bool,
    inputs: &Inputs<'_>,
) -> Sub {
    let subject = subject(text, invocation);
    if matches_any(inputs, RuleBehavior::Deny, subject, Mode::Exact)
        || matches_any(inputs, RuleBehavior::Deny, subject, Mode::Prefix)
    {
        return Check::deny(texts::bash_command_denied(text.trim())).into();
    }
    if let Some(ask) =
        ask_rule(inputs, subject, Mode::Exact).or_else(|| ask_rule(inputs, subject, Mode::Prefix))
    {
        return ask.into();
    }
    // 修复线 F68：解析不了的命令（子 shell、后台等）只认精确的 allow 规则。以前前缀与通配规则把整段原文当成一条
    // 命令比较，`echo hi; (mkdir x)` 命中 `Bash(echo:*)` 就放行。
    let allowed = matches_any(inputs, RuleBehavior::Allow, subject, Mode::Exact)
        || (invocation.is_some()
            && matches_any(inputs, RuleBehavior::Allow, subject, Mode::Prefix));
    if let Some(invocation) = invocation {
        let verdict = bash_paths::check(invocation, compound_has_cd, inputs);
        match verdict.check {
            Check::Passthrough { .. } => {}
            _ if verdict.overridable && allowed => return Check::Allow.into(),
            check => {
                let redirect = matches!(check, Check::Deny { .. })
                    && bash_paths::redirect_denied(invocation, inputs);
                return Sub {
                    check,
                    cd_with_write: verdict.cd_with_write,
                    redirect,
                };
            }
        }
    }
    if allowed {
        return Check::Allow.into();
    }
    // 工作线 F85：acceptEdits 的文件命令按去掉安全前缀之后的第一个词认（同规则的候选）；`X=1 mkdir x` 不算。
    let stripped = shell_rules::strip_safe_wrappers(text);
    let name = stripped.split_whitespace().next().unwrap_or_default();
    if (inputs.mode == PermissionMode::AcceptEdits && ACCEPT_EDITS.contains(&name))
        || is_read_only_on(text, false, inputs.paths.style)
        || invocation.is_some_and(bash_compound::test_is_read_only)
    {
        return Check::Allow.into();
    }
    Check::Passthrough {
        reason: Some(Reason::Other(texts::BASH_REQUIRES_APPROVAL.to_owned())),
        suggestions: vec![crate::suggest::suggestion(&bash_suggest::subcommand_text(
            text,
        ))],
    }
    .into()
}

/// 展示用的子命令：去掉输出重定向。
fn display(invocation: Option<&Invocation>, text: &str) -> String {
    match invocation {
        Some(invocation) if invocation.has_redirects => invocation.argv.join(" "),
        _ => text.to_owned(),
    }
}

/// 拆开的子命令（原文、解析结果、被拒绝时文字里的命令：`None` 是整条命令，§4.5.5）与结构的事实（§4.5.4）。
/// `groups` 是每一段所在的那一截管道，`group_texts` 是各截的原文（cd 按截处理，§4.5.7）。
struct Parts {
    list: Vec<(String, Option<Invocation>, Option<String>)>,
    groups: Vec<usize>,
    group_texts: Vec<String>,
    subshell: bool,
    background: bool,
}

/// 子命令：解析出错时整条命令当一个；替换里的命令按 §4.5.2 拆出，复合结构按 §4.5.4 拆开（不能静态检查时是 Err，
/// 带询问的理由与找到的简单命令）。
fn parts(command: &str, analysis: &Analysis) -> Result<Parts, (String, Vec<Invocation>)> {
    match bash_compound::split(command, analysis) {
        Split::Whole => Ok(Parts {
            list: vec![(command.to_owned(), None, None)],
            groups: vec![0],
            group_texts: vec![command.to_owned()],
            subshell: false,
            background: false,
        }),
        Split::Parts {
            parts,
            named,
            groups,
            group_texts,
            subshell,
            background,
        } => Ok(Parts {
            list: parts
                .into_iter()
                .zip(named)
                .map(|(invocation, named)| {
                    (invocation.command_text.clone(), Some(invocation), named)
                })
                .collect(),
            groups,
            group_texts,
            subshell,
            background,
        }),
        Split::Complex { reason, commands } => Err((reason, commands)),
    }
}

/// 判决时拆出的子命令数（询问框按它认复合命令，§4.10）；不能静态检查的命令没有建议，按一条算。
pub(crate) fn subcommand_count(command: &str) -> usize {
    let command = command.trim();
    parts(command, &analyze(command)).map_or(1, |parts| parts.list.len())
}

/// 去掉建议规则（§4.10 里不给建议的命令）。
fn without_suggestions(check: Check) -> Check {
    match check {
        Check::Ask { message, kind, .. } => Check::Ask {
            message,
            kind,
            suggestions: Vec::new(),
        },
        Check::Passthrough { reason, .. } => Check::Passthrough {
            reason,
            suggestions: Vec::new(),
        },
        other => other,
    }
}

/// 整条 Bash 命令的检查。
pub fn check(command: &str, inputs: &Inputs<'_>) -> Check {
    check_at(command, inputs, 0)
}

/// 同 [`check`]，`depth` 是 shell -c 脚本的嵌套层数（§4.5.9）。
pub(crate) fn check_at(command: &str, inputs: &Inputs<'_>, depth: usize) -> Check {
    let raw = command;
    let command = command.trim();
    let analysis = analyze(command);
    let result = decide(command, raw, &analysis, inputs, depth);
    // 修复（D9，工作线 F72）：有不带引号的 heredoc、参数展开或命令替换、子 shell 等结构的命令不给建议规则（规则
    // 按原文匹配，下一次的文本不同，记住了也用不上）。
    if bash_suggest::withheld(command, &analysis) {
        without_suggestions(result)
    } else {
        result
    }
}

/// `raw` 是没有去首尾空白的原文：一段被拒绝时的文字用它（§4.5.5）；整条命令命中精确的 deny 规则时去掉空白。
fn decide(
    command: &str,
    raw: &str,
    analysis: &Analysis,
    inputs: &Inputs<'_>,
    depth: usize,
) -> Check {
    let first = analysis.commands.first();
    let whole = Subject {
        plain: (analysis.commands.len() == 1)
            .then(|| first.map(|first| first.plain_text.as_str()))
            .flatten(),
        ..loose_subject(command, first)
    };
    if matches_any(inputs, RuleBehavior::Deny, whole, Mode::Exact) {
        return Check::deny(texts::bash_command_denied(command));
    }
    if let Some(ask) = ask_rule(inputs, whole, Mode::Exact) {
        return ask;
    }
    let Parts {
        list,
        groups,
        group_texts,
        subshell,
        background,
    } = match parts(command, analysis) {
        Ok(parts) => parts,
        Err((reason, commands)) => {
            return too_complex(command, raw, &commands, &reason, inputs);
        }
    };
    let placed = match cd_places(&list, &groups, &group_texts, subshell, inputs) {
        Ok(placed) => placed,
        Err(denied) => return denied,
    };
    // 每一段前面（同一截里、没有去掉的）cd 段的原文：shell -c 脚本接在它们后面判（§4.5.9）。
    let cds_before: Vec<Vec<String>> = (0..list.len())
        .map(|index| {
            (0..index)
                .filter(|&before| {
                    groups.get(before) == groups.get(index)
                        && placed[before].is_some()
                        && bash_cd::is_cd(&list[before].0, list[before].1.as_ref())
                })
                .map(|before| list[before].0.clone())
                .collect()
        })
        .collect();
    let ((parts, places), cds_before): ((Vec<_>, Vec<_>), Vec<_>) = list
        .into_iter()
        .zip(placed)
        .zip(cds_before)
        .filter_map(|((part, place), cds)| place.map(|place| ((part, place), cds)))
        .unzip();
    let subs: Vec<(String, Sub)> = parts
        .iter()
        .zip(&places)
        .map(|((text, invocation, _), place)| {
            let context = place.cwd.as_ref().map(|cwd| PathContext {
                cwd: cwd.clone(),
                ..inputs.paths.clone()
            });
            let moved = context.as_ref().map(|context| inputs.with_paths(context));
            (
                display(invocation.as_ref(), text),
                check_subcommand(
                    text,
                    invocation.as_ref(),
                    place.has_cd,
                    moved.as_ref().unwrap_or(inputs),
                ),
            )
        })
        .collect();
    if let Some(denied) = denial(&parts, &subs, raw) {
        return denied;
    }
    // 工作线 F85 追加（§4.5.9）：shell -c 脚本里有被拒绝的命令或危险删除时按危险删除一类询问；有子 shell 时不做。
    if !subshell {
        let candidates: Vec<bash_inline::Candidate<'_>> = parts
            .iter()
            .zip(&cds_before)
            .filter_map(|((_, invocation, _), cds)| {
                invocation
                    .as_ref()
                    .map(|invocation| bash_inline::Candidate {
                        invocation,
                        cds: cds.iter().map(String::as_str).collect(),
                    })
            })
            .collect();
        let joiner = if bash_cd::only_and(command) {
            " && "
        } else {
            " ; "
        };
        if let Some(asked) = bash_inline::check(&candidates, joiner, inputs, depth) {
            return asked;
        }
    }
    // 危险删除（bypass 也问）在整条命令上按它询问，不并进多操作列表。
    if let Some((_, removal)) = subs.iter().find(|(_, sub)| {
        matches!(
            sub.check,
            Check::Ask {
                kind: AskKind::Immune,
                ..
            }
        )
    }) {
        return removal.check.clone();
    }
    // 工作线 F76（§4.5.4）：有子 shell 时，拒绝与危险删除之外一律询问 shell operators；有一段命中带 content 的
    // ask 规则时性质是规则。
    if subshell {
        let rule = subs.iter().any(|(_, sub)| {
            matches!(
                sub.check,
                Check::Ask {
                    kind: AskKind::Rule,
                    ..
                }
            )
        });
        return Check::Ask {
            message: SHELL_OPERATORS.to_owned(),
            kind: if rule { AskKind::Rule } else { AskKind::Other },
            suggestions: Vec::new(),
        };
    }
    // “cd 加写入”在整条命令上询问，不并进多操作列表。
    if let Some((_, sub)) = subs.iter().find(|(_, sub)| sub.cd_with_write) {
        return sub.check.clone();
    }
    let results: Vec<(String, Check)> = subs
        .into_iter()
        .map(|(text, sub)| (text, sub.check))
        .collect();
    if results.iter().all(|(_, check)| check.is_allow()) {
        // 工作线 F76（§4.5.4）：顶层有 `&` 时每段都放行也询问一次（安全检查：yolo 放行、allow 规则盖不过）。
        // 工作线 F80（用户决定 2026-10-24“同 Node”）：guarded 不问（Node 的 guarded 只因危险命令询问）；F76 问过。
        return if background && inputs.mode != PermissionMode::Guarded {
            Check::Ask {
                message: BACKGROUND.to_owned(),
                kind: AskKind::Safety,
                suggestions: Vec::new(),
            }
        } else {
            Check::Allow
        };
    }
    if let [(_, only)] = results.as_slice() {
        return only.clone();
    }
    // 修复（工作线 F76，§4.5.4）：只有一段不放行而且它是询问时整条按它询问；
    // 以前总是列出多操作，`true && mkdir x` 的路径询问被换成了多操作的文字。
    let mut blocking = results.iter().filter(|(_, check)| !check.is_allow());
    if let (Some((_, only @ Check::Ask { .. })), None) = (blocking.next(), blocking.next()) {
        return only.clone();
    }
    // 建议按管道分段汇总（§4.10）：每条子命令的原文、它前面的操作符与结论。
    let grouped: Vec<_> = parts
        .iter()
        .zip(&results)
        .map(|((text, invocation, _), (_, check))| {
            (
                text.clone(),
                invocation
                    .as_ref()
                    .and_then(|invocation| invocation.operator_before),
                check.clone(),
            )
        })
        .collect();
    let needs: Vec<String> = results
        .iter()
        .filter(|(_, check)| !check.is_allow())
        .map(|(text, _)| text.clone())
        .collect();
    let kind = results.iter().find_map(|(_, check)| match check {
        Check::Ask {
            kind: kind @ (AskKind::Rule | AskKind::Safety | AskKind::Immune),
            ..
        } => Some(kind.clone()),
        _ => None,
    });
    let any_ask = results
        .iter()
        .any(|(_, check)| matches!(check, Check::Ask { .. }));
    let suggestions = bash_suggest::combine(&grouped);
    let reason = Reason::Subcommands(needs);
    if any_ask {
        Check::Ask {
            message: texts::request_message(TOOL, Some(&reason)),
            kind: kind.unwrap_or(AskKind::Other),
            suggestions,
        }
    } else {
        Check::Passthrough {
            reason: Some(reason),
            suggestions,
        }
    }
}

/// 一段检查时的当前目录（None 是会话的当前目录）与它所在的那一截“有没有 cd”（§4.5.7）。
struct Place {
    cwd: Option<String>,
    has_cd: bool,
}

/// 每一段的 [`Place`]；去掉的段（`cd <会话当前目录>`）是 None。cd 之后命中路径 deny 规则时 Err（文字是路径那一句）。
/// 有子 shell 或解析出错时不按截处理（对照实现先按 shell operators 询问），“有 cd”照以前看整条命令。
fn cd_places(
    list: &[(String, Option<Invocation>, Option<String>)],
    groups: &[usize],
    group_texts: &[String],
    subshell: bool,
    inputs: &Inputs<'_>,
) -> Result<Vec<Option<Place>>, Check> {
    if subshell || list.iter().any(|(_, invocation, _)| invocation.is_none()) {
        let has_cd = list.len() > 1
            && list.iter().any(|(_, invocation, _)| {
                invocation
                    .as_ref()
                    .is_some_and(|invocation| invocation.name == "cd")
            });
        return Ok(list
            .iter()
            .map(|_| Some(Place { cwd: None, has_cd }))
            .collect());
    }
    let mut places: Vec<Option<Place>> = list.iter().map(|_| None).collect();
    for (group, text) in group_texts.iter().enumerate() {
        let members: Vec<usize> = (0..list.len())
            .filter(|index| groups.get(*index) == Some(&group))
            .collect();
        if members.is_empty() {
            continue;
        }
        let entries: Vec<(&str, Option<&Invocation>)> = members
            .iter()
            .map(|&index| (list[index].0.as_str(), list[index].1.as_ref()))
            .collect();
        let plan = bash_cd::plan(text, &entries, inputs)?;
        let mut first_kept = true;
        for (position, &index) in members.iter().enumerate() {
            if plan.dropped[position] {
                continue;
            }
            let cwd = if first_kept { None } else { plan.moved.clone() };
            first_kept = false;
            places[index] = Some(Place {
                cwd,
                has_cd: plan.has_cd,
            });
        }
    }
    Ok(places)
}

/// 一段被拒绝时的结论（§4.5.5）。修复（工作线 F80，用户决定 2026-10-24“用整条命令”）：以前文字是那一段的原文
/// （路径 deny 是路径那一句）；现在是整条命令（不去首尾空白），有管道时是那一段（去掉输出重定向）。管道里的输出重定向
/// 不在各段的检查里，只有它被拒绝时保留路径那一句；别的段拒绝优先。
fn denial(
    parts: &[(String, Option<Invocation>, Option<String>)],
    subs: &[(String, Sub)],
    raw: &str,
) -> Option<Check> {
    let denied = || {
        parts
            .iter()
            .zip(subs)
            .filter(|(_, (_, sub))| matches!(sub.check, Check::Deny { .. }))
            .map(|((_, _, named), (_, sub))| (named, sub))
    };
    let Some((named, _)) = denied().find(|(named, sub)| named.is_none() || !sub.redirect) else {
        // 只有管道里的输出重定向被拒绝：保留路径那一句。
        return denied().next().map(|(_, sub)| sub.check.clone());
    };
    let command = named.as_deref().unwrap_or(raw);
    Some(Check::deny(texts::bash_command_denied(command)))
}

/// 整条命令不能静态检查时（命令替换、进程替换，§4.5.2）：整条命令的前缀 deny / ask 规则 → 每一段（含替换里面的
/// 命令）的 deny 规则，文字用整条命令（`raw`，不去首尾空白，§4.5.5）→ 危险删除 → 整条命令的精确 allow 规则 → 按理由
/// 询问（bypass 与整个工具的 allow 规则可以放行）。精确的 deny / ask 规则在 [`check`] 开头已经查过。
fn too_complex(
    command: &str,
    raw: &str,
    commands: &[Invocation],
    reason: &str,
    inputs: &Inputs<'_>,
) -> Check {
    let whole = loose_subject(command, commands.first());
    if matches_any(inputs, RuleBehavior::Deny, whole, Mode::Prefix) {
        return Check::deny(texts::bash_command_denied(raw));
    }
    if matches_any(inputs, RuleBehavior::Ask, whole, Mode::Prefix) {
        return Check::Ask {
            message: texts::request_message(TOOL, None),
            kind: AskKind::Rule,
            suggestions: Vec::new(),
        };
    }
    let segments = bash_subst::segments(commands);
    let denied = segments.iter().any(|segment| {
        let subject = Subject {
            plain: Some(segment.plain_text.as_str()),
            ..loose_subject(&segment.command_text, Some(segment))
        };
        matches_any(inputs, RuleBehavior::Deny, subject, Mode::Exact)
            || matches_any(inputs, RuleBehavior::Deny, subject, Mode::Prefix)
    });
    if denied {
        return Check::deny(texts::bash_command_denied(raw));
    }
    let has_cd = segments.len() > 1 && segments.iter().any(|segment| segment.name == "cd");
    if let Some(removal) = segments
        .iter()
        .find_map(|segment| removal::check(segment, has_cd, inputs))
    {
        return removal;
    }
    if matches_any(inputs, RuleBehavior::Allow, whole, Mode::Exact) {
        return Check::Allow;
    }
    Check::Ask {
        message: reason.to_owned(),
        kind: AskKind::Other,
        suggestions: Vec::new(),
    }
}

#[cfg(test)]
#[path = "bash_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "bash_subst_tests.rs"]
mod subst_tests;

#[cfg(test)]
#[path = "removal_tests.rs"]
mod removal_tests;

#[cfg(test)]
#[path = "bash_compound_tests.rs"]
mod compound_tests;

#[cfg(test)]
#[path = "bash_denied_tests.rs"]
mod denied_tests;

#[cfg(test)]
#[path = "bash_rule_forms_tests.rs"]
mod rule_forms_tests;

#[cfg(test)]
#[path = "bash_cd_tests.rs"]
mod cd_tests;

#[cfg(test)]
#[path = "bash_inline_tests.rs"]
mod inline_tests;
