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

/// 复合命令里有 cd，又按相对路径读，而且配置了 Read deny 规则（§4.5.7，工作线 F85；前面接命令名）。
pub const CD_RELATIVE_READ: &str = "reads a file by a relative path after a cd in a compound command; which file that is cannot be resolved statically while a Read() deny rule is configured, so this needs approval.";

/// 不能解析的 cd 之后读的相对路径落在 deny 规则的目录下（§4.5.7，工作线 F85）。
pub fn unresolved_cd_denied(command: &str, path: &str, rule: &str) -> String {
    format!(
        "{command} reads '{path}' after a cd whose target cannot be resolved, and a deny rule ({rule}) names that file under a directory it may run in"
    )
}

/// 输入重定向的目标有 glob，或在 cd 之后是相对路径（§4.5.8，工作线 F85）。
pub fn input_unresolved(target: &str) -> String {
    format!(
        "Input redirection from '{target}' requires manual approval: the file it names depends on glob expansion or a directory change in the same command."
    )
}

/// 输入重定向读工作目录以外的文件（§4.5.8，工作线 F85）。
pub fn input_outside(path: &str, dirs: &str) -> String {
    format!(
        "Input redirection from '{path}' was blocked. For security, ZCode may only read files in the allowed working directories for this session: {dirs}."
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

/// 危险删除的询问都以这一句收尾（shell-permissions.md §4.5.3）。
const REMOVAL_TAIL: &str =
    "This requires explicit approval and cannot be auto-allowed by permission rules.";

/// 危险删除：字面目标命中的情况。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Removal {
    /// 复合命令里有 cd，相对的 glob 目标定不下来。
    ChangesDirectories,
    /// glob 目标定不到一个目录。
    Unresolved,
    /// 关键目录（根、根下的一级目录、home）。
    Critical,
    /// 工作目录或它的上级。
    Workspace,
    /// glob 跨了多层目录。
    Traverses,
}

/// 字面目标的危险删除。`command` 是 `rm` 或 `rmdir`，`target` 是解析后的目标。
pub fn dangerous_removal(command: &str, target: &str, why: Removal) -> String {
    let why = match why {
        Removal::ChangesDirectories => {
            "This command changes directories before the removal, so the relative glob target cannot be statically resolved."
        }
        Removal::Unresolved => {
            "This command's removal target cannot be statically resolved to a directory."
        }
        Removal::Critical => "This command would remove a critical system directory.",
        Removal::Workspace => {
            "This command would remove a workspace directory (the working directory, an additional working directory, or one of their parent directories)."
        }
        Removal::Traverses => {
            "This command's glob pattern traverses directories that cannot be statically enumerated."
        }
    };
    format!("Dangerous {command} operation detected: '{target}'\n\n{why} {REMOVAL_TAIL}")
}

/// Windows 上只有反斜杠的目标（Git Bash 里是盘符根）。
pub fn dangerous_backslash_removal(command: &str, target: &str) -> String {
    format!(
        "Dangerous {command} operation detected: '{target}'\n\nA backslash-only target is the drive root in Git Bash on Windows. {REMOVAL_TAIL}\n\nWrite the directory you mean as a path, for example /c/work/build or C:/work/build."
    )
}

/// `rm -r` 的目标整个是命令替换的输出。
pub fn dangerous_substitution_removal() -> String {
    format!(
        "Dangerous rm operation detected: the target is the output of a command substitution (`$(...)` or backticks) and cannot be checked before the command runs. {REMOVAL_TAIL}\n\nRun the substitution on its own first, then remove the literal paths it prints."
    )
}

/// 目标是变量为空时会变成根（`root_child` 为 None）或根下一级目录的展开。`names` 是 `$A or $B`。
pub fn dangerous_variable_removal(
    command: &str,
    invocation: &str,
    target: &str,
    names: &str,
    root_child: Option<&str>,
    advice: &str,
) -> String {
    let (becomes, reach) = match root_child {
        None => (
            "`/`, `/*` or a top-level path".to_owned(),
            "the filesystem root",
        ),
        Some(child) => (format!("`/{child}`"), "a top-level directory"),
    };
    format!(
        "Dangerous {command} operation detected in `{invocation}`. The target '{target}' is a shell variable expansion: when {names} is unset or empty it becomes {becomes}. {REMOVAL_TAIL}\n\nThis check does not fire on a target that cannot expand to {reach}: {advice}."
    )
}
