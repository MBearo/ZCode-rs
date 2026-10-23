//! Bash 的权限检查（shell-permissions.md §4.5）。
//!
//! 整条命令先比精确规则；再按子命令（`&&`、`||`、`;`、`|`，加上从替换里拆出的命令，§4.5.2）逐个检查：deny 前缀 →
//! ask 前缀 → 路径（含危险删除，§4.5.3）→ 精确 / 前缀 allow → acceptEdits 下的文件命令 → 只读命令放行 →
//! “需要批准”。全部放行才放行；任何一个拒绝就拒绝；危险删除与“cd 加写入”按那个子命令询问；复合命令的其余询问
//! 列出需要批准的部分。

use rust_cli_kernel_api::permission::{PermissionMode, PermissionRule, RuleBehavior};

use crate::bash_subst::{self, Expanded};
use crate::check::{AskKind, Check, Inputs};
use crate::parse::{Invocation, analyze};
use crate::readonly::is_read_only;
use crate::shell_rules::{self, Mode};
use crate::texts::{self, Reason};
use crate::{bash_paths, removal};

const TOOL: &str = "Bash";
/// acceptEdits 下自动放行的文件命令。
const ACCEPT_EDITS: &[&str] = &["mkdir", "touch", "rm", "rmdir", "mv", "cp", "sed"];
/// 不按前缀建议规则的命令（解释器与 shell：前缀等于放行任意代码）。
const BARE: &[&str] = &[
    "sh", "bash", "zsh", "fish", "python", "python3", "node", "deno", "bun", "ruby", "perl", "php",
    "env", "xargs", "sudo", "eval", "exec",
];

/// 命中的 ask 规则换成询问。
fn ask_rule(inputs: &Inputs<'_>, candidates: &[String], mode: Mode) -> Option<Check> {
    let (_, found) = inputs
        .rules
        .contents(TOOL, RuleBehavior::Ask)
        .into_iter()
        .find(|(content, _)| shell_rules::matches(content, candidates, mode, |_| false))?;
    Some(Check::Ask {
        message: texts::request_message(TOOL, Some(&Reason::Rule(found.text(), found.source))),
        kind: AskKind::Rule,
        suggestions: Vec::new(),
    })
}

fn matches_any(
    inputs: &Inputs<'_>,
    behavior: RuleBehavior,
    candidates: &[String],
    mode: Mode,
) -> bool {
    inputs
        .rules
        .contents(TOOL, behavior)
        .iter()
        .any(|(content, _)| shell_rules::matches(content, candidates, mode, |_| false))
}

/// 询问时建议的规则（简化）：`cmd sub:*` 或 `cmd:*`，解释器与
/// 多行命令给精确规则。
pub fn suggestion(command: &str) -> PermissionRule {
    let words: Vec<&str> = command.split_whitespace().collect();
    let exact = || PermissionRule {
        tool: TOOL.to_owned(),
        content: Some(command.to_owned()),
    };
    let Some(first) = words.first() else {
        return exact();
    };
    if command.contains('\n') || BARE.contains(first) || words.len() == 1 {
        return exact();
    }
    let second = words.get(1).filter(|word| {
        word.chars().next().is_some_and(|c| c.is_ascii_lowercase())
            && word
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    });
    let prefix = match second {
        Some(second) => format!("{first} {second}"),
        None => (*first).to_owned(),
    };
    PermissionRule {
        tool: TOOL.to_owned(),
        content: Some(format!("{prefix}:*")),
    }
}

/// 一个子命令的结论；`cd_with_write` 标出“复合命令里有 cd 又有写入”的询问。
struct Sub {
    check: Check,
    cd_with_write: bool,
}

impl From<Check> for Sub {
    fn from(check: Check) -> Self {
        Self {
            check,
            cd_with_write: false,
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
    let candidates = shell_rules::candidates(text);
    if matches_any(inputs, RuleBehavior::Deny, &candidates, Mode::Exact)
        || matches_any(inputs, RuleBehavior::Deny, &candidates, Mode::Prefix)
    {
        return Check::deny(texts::bash_command_denied(text.trim())).into();
    }
    if let Some(ask) = ask_rule(inputs, &candidates, Mode::Exact)
        .or_else(|| ask_rule(inputs, &candidates, Mode::Prefix))
    {
        return ask.into();
    }
    // 修复线 F68：解析不了的命令（子 shell、后台等）只认精确的 allow 规则。以前前缀与通配规则把整段原文当成一条
    // 命令比较，`echo hi; (mkdir x)` 命中 `Bash(echo:*)` 就放行。
    let allowed = matches_any(inputs, RuleBehavior::Allow, &candidates, Mode::Exact)
        || (invocation.is_some()
            && matches_any(inputs, RuleBehavior::Allow, &candidates, Mode::Prefix));
    if let Some(invocation) = invocation {
        let verdict = bash_paths::check(invocation, compound_has_cd, inputs);
        match verdict.check {
            Check::Passthrough { .. } => {}
            _ if verdict.overridable && allowed => return Check::Allow.into(),
            check => {
                return Sub {
                    check,
                    cd_with_write: verdict.cd_with_write,
                };
            }
        }
    }
    if allowed {
        return Check::Allow.into();
    }
    let name = invocation
        .and_then(|invocation| {
            crate::readonly::strip_wrappers(&invocation.argv)
                .first()
                .cloned()
        })
        .unwrap_or_default();
    if (inputs.mode == PermissionMode::AcceptEdits && ACCEPT_EDITS.contains(&name.as_str()))
        || is_read_only(text, false)
    {
        return Check::Allow.into();
    }
    Check::Passthrough {
        reason: Some(Reason::Other(texts::BASH_REQUIRES_APPROVAL.to_owned())),
        suggestions: vec![suggestion(text.trim())],
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

/// 整条 Bash 命令的检查。
pub fn check(command: &str, inputs: &Inputs<'_>) -> Check {
    let command = command.trim();
    let whole = shell_rules::candidates(command);
    if matches_any(inputs, RuleBehavior::Deny, &whole, Mode::Exact) {
        return Check::deny(texts::bash_command_denied(command));
    }
    if let Some(ask) = ask_rule(inputs, &whole, Mode::Exact) {
        return ask;
    }
    let analysis = analyze(command);
    let parsed = !analysis.has_parse_errors
        && !analysis.has_unsupported_syntax
        && !analysis.commands.is_empty();
    let parts: Vec<(String, Option<Invocation>)> = if parsed {
        match bash_subst::expand(&analysis.commands) {
            Expanded::Parts(parts) => parts
                .into_iter()
                .map(|invocation| (invocation.command_text.clone(), Some(invocation)))
                .collect(),
            Expanded::Complex(reason) => {
                return too_complex(command, &analysis.commands, reason, inputs);
            }
        }
    } else {
        vec![(command.to_owned(), None)]
    };
    let compound_has_cd = parts.len() > 1
        && parts.iter().any(|(_, invocation)| {
            invocation
                .as_ref()
                .is_some_and(|invocation| invocation.name == "cd")
        });
    let subs: Vec<(String, Sub)> = parts
        .iter()
        .map(|(text, invocation)| {
            (
                display(invocation.as_ref(), text),
                check_subcommand(text, invocation.as_ref(), compound_has_cd, inputs),
            )
        })
        .collect();
    if let Some((_, denied)) = subs
        .iter()
        .find(|(_, sub)| matches!(sub.check, Check::Deny { .. }))
    {
        return denied.check.clone();
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
    // “cd 加写入”在整条命令上询问，不并进多操作列表。
    if let Some((_, sub)) = subs.iter().find(|(_, sub)| sub.cd_with_write) {
        return sub.check.clone();
    }
    let results: Vec<(String, Check)> = subs
        .into_iter()
        .map(|(text, sub)| (text, sub.check))
        .collect();
    if let [(_, only)] = results.as_slice() {
        return only.clone();
    }
    if results.iter().all(|(_, check)| check.is_allow()) {
        return Check::Allow;
    }
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
    let suggestions = needs.iter().map(|text| suggestion(text)).take(5).collect();
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

/// 整条命令不能静态检查时（命令替换、进程替换，§4.5.2）：整条命令的前缀 deny / ask 规则 → 每一段（含替换里面的
/// 命令）的 deny 规则，文字用整条命令 → 危险删除 → 整条命令的精确 allow 规则 → 按理由询问（bypass 与整个工具的 allow
/// 规则可以放行）。精确的 deny / ask 规则在 [`check`] 开头已经查过。
fn too_complex(
    command: &str,
    commands: &[Invocation],
    reason: &'static str,
    inputs: &Inputs<'_>,
) -> Check {
    let whole = shell_rules::candidates(command);
    if matches_any(inputs, RuleBehavior::Deny, &whole, Mode::Prefix) {
        return Check::deny(texts::bash_command_denied(command));
    }
    if matches_any(inputs, RuleBehavior::Ask, &whole, Mode::Prefix) {
        return Check::Ask {
            message: texts::request_message(TOOL, None),
            kind: AskKind::Rule,
            suggestions: Vec::new(),
        };
    }
    let segments = bash_subst::segments(commands);
    let denied = segments.iter().any(|segment| {
        let candidates = shell_rules::candidates(&segment.command_text);
        matches_any(inputs, RuleBehavior::Deny, &candidates, Mode::Exact)
            || matches_any(inputs, RuleBehavior::Deny, &candidates, Mode::Prefix)
    });
    if denied {
        return Check::deny(texts::bash_command_denied(command));
    }
    let has_cd = segments.len() > 1 && segments.iter().any(|segment| segment.name == "cd");
    if let Some(removal) = segments
        .iter()
        .find_map(|segment| removal::check(segment, has_cd, inputs))
    {
        return removal;
    }
    if matches_any(inputs, RuleBehavior::Allow, &whole, Mode::Exact) {
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
