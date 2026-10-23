//! 命令替换与进程替换（shell-permissions.md §4.5.2）。
//!
//! 双引号里的命令替换（字符串里另有至少两个字面字符）、整个赋值的值就是一个命令替换时，里面的命令拆成单独的子命令，
//! 排在外层命令前面，规则要都匹配才放行；其余替换（不在双引号里的参数、几乎只有替换的双引号字符串、重定向目标、
//! 进程替换、藏在别的展开里的命令）让整条命令不能静态检查：
//! 只有 deny 规则、前缀 ask 规则、危险删除与整条命令的精确 allow 规则起作用，其余询问。
//!
//! 修复线 F68（审计 P4）：以前子命令的文字整体与 allow 规则比较，`echo $(mkdir x)` 命中 `Bash(echo:*)` 就放行，
//! 替换里的命令照样执行。

use crate::parse::{Invocation, SubstitutionKind, analyze};

/// 命令替换让整条命令不能静态检查时的理由。
pub(crate) const NESTED: &str = "A nested command in this command can't be checked before it runs";
/// 进程替换的理由。
pub(crate) const AS_FILE: &str =
    "A command used as a file in this command can't be checked before it runs";
/// 双引号字符串几乎只有命令替换（`"$(cmd)"`）时的理由。
pub(crate) const QUOTED: &str = "Quoted text in this command can't be checked before it runs";
/// 拆替换的最多层数，超过按不能检查处理。
const MAX_DEPTH: usize = 8;

/// 拆替换的结果。
pub(crate) enum Expanded {
    /// 子命令：替换里拆出的命令排在它所在的外层命令前面。
    Parts(Vec<Invocation>),
    /// 整条命令不能静态检查：询问的理由。
    Complex(&'static str),
}

/// 按出现的顺序拆开每条命令里的替换。
pub(crate) fn expand(commands: &[Invocation]) -> Expanded {
    expand_at(commands, 0)
}

fn expand_at(commands: &[Invocation], depth: usize) -> Expanded {
    let mut parts = Vec::new();
    for invocation in commands {
        for substitution in &invocation.substitutions {
            if !substitution.splittable {
                return Expanded::Complex(match substitution.kind {
                    SubstitutionKind::Process => AS_FILE,
                    SubstitutionKind::Quoted => QUOTED,
                    SubstitutionKind::Command | SubstitutionKind::Hidden => NESTED,
                });
            }
            let inner = analyze(&substitution.inner);
            if depth >= MAX_DEPTH || inner.has_parse_errors || inner.has_unsupported_syntax {
                return Expanded::Complex(NESTED);
            }
            match expand_at(&inner.commands, depth + 1) {
                Expanded::Parts(found) => parts.extend(found),
                complex @ Expanded::Complex(_) => return complex,
            }
        }
        parts.push(invocation.clone());
    }
    Expanded::Parts(parts)
}

/// 整条命令里能找到的全部简单命令，含命令替换里面的（解析得了的才算）：不能静态检查时 deny 规则与危险删除
/// 逐个看它们。
pub(crate) fn segments(commands: &[Invocation]) -> Vec<Invocation> {
    let mut found = Vec::new();
    collect(commands, 0, &mut found);
    found
}

fn collect(commands: &[Invocation], depth: usize, found: &mut Vec<Invocation>) {
    for invocation in commands {
        found.push(invocation.clone());
        if depth >= MAX_DEPTH {
            continue;
        }
        for substitution in &invocation.substitutions {
            if substitution.inner.is_empty() {
                continue;
            }
            let inner = analyze(&substitution.inner);
            if !inner.has_parse_errors && !inner.has_unsupported_syntax {
                collect(&inner.commands, depth + 1, found);
            }
        }
    }
}
