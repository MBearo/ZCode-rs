//! 文件工具的权限（shell-permissions.md §4.4）。
//!
//! 读：deny 规则 → ask 规则 → 工作目录、内部目录或本会话计划文件里放行 → allow 规则 → 询问。
//! 写：deny 规则 → 本会话计划文件放行（ask 规则除外）→ ask 规则 → 安全路径询问（allow 规则盖不过，bypass 放行）→
//! plan 模式询问 → acceptEdits 下工作目录里放行 → allow 规则 → 询问。
//! 读规则的工具名是 `Read`，写规则是 `Edit`（Write、NotebookEdit 共用）。

use rust_cli_base::names::DOT_DIR;
use rust_cli_kernel_api::env_files::PathStyle;
use rust_cli_kernel_api::permission::{PermissionMode, RuleBehavior};
use serde_json::Value;

use crate::check::{AskKind, Check, Inputs};
use crate::paths::rule_matches;
use crate::texts;

/// 文件访问的方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
}

impl Access {
    /// 路径规则的工具名。
    pub fn rule_tool(self) -> &'static str {
        match self {
            Access::Read => "Read",
            Access::Write => "Edit",
        }
    }
}

/// 内置文件工具的访问方向与路径字段。`path` 缺省时（Glob、Grep）是 cwd。
pub fn tool_access(tool: &str) -> Option<(Access, &'static str)> {
    Some(match tool {
        "Read" => (Access::Read, "file_path"),
        "Glob" | "Grep" => (Access::Read, "path"),
        "Edit" | "Write" => (Access::Write, "file_path"),
        "NotebookEdit" => (Access::Write, "notebook_path"),
        _ => return None,
    })
}

/// 写入要额外确认的位置（危险的文件与目录，配置目录是 `.zcode`）。
const SENSITIVE_DIRS: &[&str] = &[".git", ".vscode", ".idea", DOT_DIR];
const SENSITIVE_FILES: &[&str] = &[
    ".gitconfig",
    ".gitmodules",
    ".bashrc",
    ".bash_profile",
    ".zshrc",
    ".zprofile",
    ".profile",
    ".ripgreprc",
    ".mcp.json",
    // 项目配置 `zcode.json`（带 MCP 服务器与 hooks）。修复线 F58：以前是机械替换出来的
    // `.zcode.json`（zcode 没有这个文件），真正的项目配置不在表里；用户级配置在 `~/.zcode/` 下，目录规则已覆盖。
    rust_cli_base::names::PROJECT_CONFIG_FILE,
];

fn sensitive(path: &str, windows: bool) -> bool {
    let normalized = if windows {
        path.replace('\\', "/").to_lowercase()
    } else {
        path.to_owned()
    };
    let segments: Vec<&str> = normalized
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    let Some((file, dirs)) = segments.split_last() else {
        return false;
    };
    dirs.iter().any(|dir| SENSITIVE_DIRS.contains(dir)) || SENSITIVE_FILES.contains(file)
}

/// 路径命中某种行为的路径规则。
pub fn path_rule(access: Access, path: &str, behavior: RuleBehavior, inputs: &Inputs<'_>) -> bool {
    inputs
        .rules
        .contents(access.rule_tool(), behavior)
        .iter()
        .any(|(pattern, _)| rule_matches(pattern, path, inputs.root, inputs.paths, behavior))
}

/// 检查一次读或写。`path` 是工具输入里的原样路径（None 表示 cwd）。
pub fn check(access: Access, path: Option<&str>, inputs: &Inputs<'_>) -> Check {
    let paths = inputs.paths;
    // 缺陷修复（F14）：工具改成去首尾空白、做 NFC 之后，判定若仍用 `resolve`，
    // `" /etc/hosts"` 会被当成工作区里的相对路径放行，而工具读的是 `/etc/hosts`。两边用同一条规则。
    let resolved = paths.resolve_input(path.unwrap_or("."));
    let rule = |behavior| path_rule(access, &resolved, behavior, inputs);
    match access {
        Access::Read => {
            if rule(RuleBehavior::Deny) {
                return Check::deny(texts::file_read_denied(&resolved));
            }
            if rule(RuleBehavior::Ask) {
                return ask(texts::file_read_request(&resolved), AskKind::Rule);
            }
            if paths.in_working_dirs(&resolved)
                || paths.internal_readable(&resolved)
                || paths.is_plan_file(&resolved)
                || rule(RuleBehavior::Allow)
                || path_rule(Access::Write, &resolved, RuleBehavior::Allow, inputs)
            {
                return Check::Allow;
            }
            ask(texts::file_read_request(&resolved), AskKind::Other)
        }
        Access::Write => {
            if rule(RuleBehavior::Deny) {
                return Check::deny(texts::file_edit_denied(&resolved));
            }
            // 本会话的计划文件（tools-more.md §2.5）：不看模式，排在安全路径之前
            // （计划目录在 `.zcode` 下）；ask 规则仍然生效（先查 ask 规则再查计划文件）。
            if paths.is_plan_file(&resolved) {
                if rule(RuleBehavior::Ask) {
                    return ask(texts::file_write_request(&resolved), AskKind::Rule);
                }
                return Check::Allow;
            }
            // 修复线 F68：ask 规则排在敏感文件之前。敏感文件的询问在 bypass 下放行之后，原来的顺序会让
            // `Edit(.bashrc)` 这类 ask 规则在 bypass 下失效。
            if rule(RuleBehavior::Ask) {
                return ask(texts::file_write_request(&resolved), AskKind::Rule);
            }
            if sensitive(&resolved, paths.style == PathStyle::Windows) {
                return ask(texts::file_sensitive(&resolved), AskKind::Safety);
            }
            // plan 模式下其余写入一律询问，排在 acceptEdits 与 allow 规则之前；
            // 外层再按 plan 模式交分类器或询问用户。
            if inputs.mode == PermissionMode::Plan {
                return ask(texts::file_plan_mode(&resolved), AskKind::Other);
            }
            let accepted =
                inputs.mode == PermissionMode::AcceptEdits && paths.in_working_dirs(&resolved);
            if accepted || rule(RuleBehavior::Allow) {
                return Check::Allow;
            }
            ask(texts::file_write_request(&resolved), AskKind::Other)
        }
    }
}

fn ask(message: String, kind: AskKind) -> Check {
    Check::Ask {
        message,
        kind,
        suggestions: Vec::new(),
    }
}

/// 工具输入里的路径字段。
pub fn input_path<'a>(input: &'a Value, field: &str) -> Option<&'a str> {
    input
        .get(field)
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "files_plan_tests.rs"]
mod plan_tests;
