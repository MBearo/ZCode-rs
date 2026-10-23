//! 判决链给模型与用户看的固定文本（docs/specs/rust-cli/prompts.md 规则 4：短提示语暂放在各 crate
//! 的 texts 模块）。文字逐字固定，测试按字面比较。询问的四句（`ZCode requested permissions …`）说产品名 ZCode，
//! 与执行管线里没有规则时的说法一致（修复线 F51b，shell-permissions.md §4.8）。

use rust_cli_kernel_api::permission::RuleSource;

/// 变通说明的正文，末尾带空格。
const WORKAROUND: &str = "IMPORTANT: You *may* attempt to accomplish this action using other tools that might naturally be used to accomplish this goal, e.g. using head instead of cat. But you *should not* attempt to work around this denial in malicious ways, e.g. do not use your ability to run tests to execute non-test actions. You should only try to work around this restriction in reasonable ways that do not attempt to bypass the intent behind this denial. ";

/// 拒绝之后的变通说明。
pub fn denial_workaround() -> String {
    format!(
        "{WORKAROUND}If you believe this capability is essential to complete the user's request, STOP and explain to the user what you were trying to do and why you need this permission. Let the user decide how to proceed."
    )
}

/// dontAsk 模式把询问换成拒绝。
pub fn dont_ask_rejected(tool: &str) -> String {
    format!(
        "Permission to use {tool} has been denied because ZCode is running in don't ask mode. {}",
        denial_workaround()
    )
}

/// 不能询问的上下文（`-p` 里的后台子代理）把询问换成拒绝（agents.md §4.1.1）。
pub fn prompts_unavailable(tool: &str) -> String {
    format!(
        "Permission to use {tool} has been denied. {}",
        denial_workaround()
    )
}

/// 整个工具被 deny 规则拒绝。
pub fn tool_denied(tool: &str) -> String {
    format!("Permission to use {tool} has been denied.")
}

/// Bash 命令命中 deny 规则。
pub fn bash_command_denied(command: &str) -> String {
    format!("Permission to use Bash with command {command} has been denied.")
}

/// Bash 子命令的默认询问理由。
pub const BASH_REQUIRES_APPROVAL: &str = "This command requires approval";

/// guarded 下的危险命令（Node `permission/service.ts` 的原文，修复线 F20，shell-permissions.md §4.6.1）。
pub const GUARDED_ASK: &str = "Dangerous command requires your approval for this execution";

/// 规则来源的说明。
pub fn source_display(source: RuleSource) -> &'static str {
    match source {
        RuleSource::UserSettings => "user settings",
        RuleSource::ProjectSettings => "shared project settings",
        RuleSource::LocalSettings => "project local settings",
        RuleSource::CliArg => "CLI argument",
        RuleSource::Session => "current session",
    }
}

/// 询问的理由（与文本有关的部分）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    /// 命中 ask 规则：`(规则串, 来源)`。
    Rule(String, RuleSource),
    /// 复合命令里需要批准的子命令（展示时去掉输出重定向）。
    Subcommands(Vec<String>),
    /// 其他理由，原样作为文本（其他、安全检查、工作目录）。
    Other(String),
    /// sandbox 之外运行。
    SandboxOverride,
}

fn plural(count: usize, one: &str, many: &str) -> String {
    if count == 1 { one } else { many }.to_owned()
}

/// 权限请求的说明。
pub fn request_message(tool: &str, reason: Option<&Reason>) -> String {
    match reason {
        None => {
            format!("ZCode requested permissions to use {tool}, but you haven't granted it yet.")
        }
        Some(Reason::Rule(rule, source)) => format!(
            "Permission rule '{rule}' from {} requires approval for this {tool} command",
            source_display(*source)
        ),
        Some(Reason::Subcommands(parts)) if parts.is_empty() => {
            format!("This {tool} command contains multiple operations that require approval")
        }
        Some(Reason::Subcommands(parts)) => format!(
            "This {tool} command contains multiple operations. The following {} {} approval: {}",
            plural(parts.len(), "part", "parts"),
            plural(parts.len(), "requires", "require"),
            parts.join(", ")
        ),
        Some(Reason::Other(text)) => text.clone(),
        Some(Reason::SandboxOverride) => "Run outside of the sandbox".to_owned(),
    }
}

/// 自动模式的分类器不可用（加后续说明）。
pub const CLASSIFIER_UNAVAILABLE: &str = "Auto mode could not evaluate this action and is blocking it for safety — run with --debug for details. This is not a judgment that the action is unsafe. This is usually transient — wait briefly and try this action again as-is; don't rewrite it. If it keeps failing, continue with other tasks that don't require this action and come back to it later. Note: reading files, searching code, and other read-only operations do not require the classifier and can still be used.";

/// 分类器拦下（带结尾的说明）。
pub fn classifier_blocked(reason: &str) -> String {
    format!(
        "Permission for this action was denied by the ZCode auto mode classifier. Reason: {reason}. If you have other tasks that don't depend on this action, continue working on those. {WORKAROUND}If you believe this capability is essential to complete the user's request, first try a safer method. Get as much of the rest of the task done as you can, then STOP and explain to the user what you were trying to do and why you need this permission. Let the user decide how to proceed."
    )
}

/// 工作目录列表，最多列 5 个，其余写成 “and N more”。
pub fn directories(dirs: &[String]) -> String {
    const SHOWN: usize = 5;
    let quoted: Vec<String> = dirs
        .iter()
        .take(SHOWN)
        .map(|dir| format!("'{dir}'"))
        .collect();
    if dirs.len() <= SHOWN {
        quoted.join(", ")
    } else {
        format!("{}, and {} more", quoted.join(", "), dirs.len() - SHOWN)
    }
}

/// 工作目录里的写入也要批准。
pub fn write_inside(subject: &str, dirs: &str) -> String {
    format!(
        "{subject} needs approval. The path is inside the working directories for this session ({dirs}), and ZCode asks before a shell command creates, changes or removes files there."
    )
}

/// 工作目录以外的写入。
pub fn write_outside(subject: &str, dirs: &str) -> String {
    format!(
        "{subject} needs approval. The path is outside the working directories for this session ({dirs}). Allowing runs the command as written."
    )
}

/// 读工作目录以外的路径。`action` 是命令的动作说明。
pub fn read_outside(command: &str, path: &str, action: &str, dirs: &str) -> String {
    format!(
        "{command} in '{path}' was blocked. For security, ZCode may only {action} the allowed working directories for this session: {dirs}."
    )
}

/// 路径命中 deny 规则。
pub fn path_rule_denied(command: &str, preposition: &str, path: &str) -> String {
    format!("{command} {preposition} '{path}' was blocked by a deny rule.")
}

/// 写入目标是运行时才知道的（命令替换、变量）。
pub fn runtime_target(command: &str) -> String {
    format!(
        "{command} target contains command-substitution or untracked-variable output — the path is runtime-determined and cannot be validated"
    )
}

/// 复合命令里有 cd 又有写入。
pub const CD_WITH_WRITE: &str = "Commands that change directories and perform write operations require explicit approval to ensure paths are evaluated correctly. For security, ZCode cannot automatically determine the final working directory when 'cd' is used in compound commands.";

/// 文件工具的读写询问与拒绝。
pub fn file_read_request(path: &str) -> String {
    format!("ZCode requested permissions to read from {path}, but you haven't granted it yet.")
}

pub fn file_write_request(path: &str) -> String {
    format!("ZCode requested permissions to write to {path}, but you haven't granted it yet.")
}

/// plan 模式下的写入。
pub fn file_plan_mode(path: &str) -> String {
    format!("Cannot write to {path} while in plan mode.")
}

pub fn file_sensitive(path: &str) -> String {
    format!("ZCode requested permissions to edit {path} which is a sensitive file.")
}

pub fn file_read_denied(path: &str) -> String {
    format!("Permission to read {path} has been denied.")
}

pub fn file_edit_denied(path: &str) -> String {
    format!("Permission to edit {path} has been denied.")
}
