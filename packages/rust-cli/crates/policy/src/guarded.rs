// 移植自 apps/zcode-cli/packages/core/src/tool/handlers/guarded/command.ts（修复线 F20）。
//! guarded 模式的危险命令识别（Node `matchDangerousCommand`，docs/specs/rust-cli/shell-permissions.md §4.6.1）。
//!
//! 命中时判决链先询问一次（只批准这一次），没命中或认不出时同 bypassPermissions 放行。认不出（Unsupported）与
//! 没命中（NotMatched）只在测试里区分，判决只看命中。

mod find;
mod git;
mod options;
mod word;

use rust_cli_kernel_api::env_files::PathStyle;
use rust_cli_kernel_api::permission::PermissionMode;
use serde_json::Value;

use options::{COMMAND_SCAN, Hit, Scan, Table, enabled, parse, settle, unwrap};
use word::{Word, basename};

/// Node `ExecutionShellSelection.dialect` 里本工程用得到的两种：Bash 工具总用 bash，Windows 上是 Git Bash。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    Posix,
    GitBash,
}

impl Dialect {
    /// 执行环境的路径风格是 Windows 时按 Git Bash。
    pub fn of(style: PathStyle) -> Self {
        match style {
            PathStyle::Posix => Self::Posix,
            PathStyle::Windows => Self::GitBash,
        }
    }
}

/// 一条 Bash 命令的识别结果（Node `DangerousCommandResult`，去掉位置）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Danger {
    /// 规则号 `safety.bash.<id>`。
    Matched(String),
    NotMatched,
    Unsupported,
}

const RM: Table = Table {
    flags: "-r -R --recursive -f --force -i -I --one-file-system --preserve-root --no-preserve-root -v --verbose -d --dir --help --version",
    values: "",
    optional: "--interactive",
};
const RSYNC_DELETE: [&str; 7] = [
    "--delete",
    "--del",
    "--delete-before",
    "--delete-during",
    "--delete-delay",
    "--delete-after",
    "--delete-excluded",
];
const RSYNC: Table = Table {
    flags: "-a -r -v -q -z -h -P -p -t -g -o -D -l -H -A -X -u -c -n --dry-run --archive --recursive --verbose --quiet --compress --progress --partial --checksum --update --numeric-ids --ignore-errors --force --help --version --delete --del --delete-before --delete-during --delete-delay --delete-after --delete-excluded",
    values: "-e --rsh -f --filter --include --exclude --include-from --exclude-from --files-from --rsync-path --log-file --out-format --backup-dir --suffix --chmod --chown --max-delete --timeout --bwlimit --compare-dest --copy-dest --link-dest",
    optional: "",
};
const INTERPRETERS: [&str; 14] = [
    "bash",
    "sh",
    "zsh",
    "cmd",
    "powershell",
    "pwsh",
    "eval",
    "source",
    ".",
    "python",
    "python3",
    "node",
    "perl",
    "ruby",
];
/// Node `MAX_LENGTH`（UTF-16 码元）。
const MAX_LENGTH: usize = 10_000;

/// `mkfs.<名字>`（`^mkfs\.[a-zA-Z0-9_-]+$`）。
fn mkfs_variant(name: &str) -> bool {
    name.strip_prefix("mkfs.").is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    })
}

/// Node `matchInvocation`。
fn invocation(name: &str, args: &[Word], dialect: Dialect) -> Hit {
    if name == "git" {
        return git::matches(args);
    }
    if INTERPRETERS.contains(&name) {
        return Hit::Unknown;
    }
    if name == "find" {
        return find::find_delete(args);
    }
    if dialect == Dialect::GitBash && name == "robocopy" {
        return find::robocopy_delete(args);
    }
    if name == "rm" {
        let parsed = parse(args, &RM, COMMAND_SCAN);
        // BSD rm 在第一个目标之后不再解析选项：目标按这个语义单独数，危险选项仍按 GNU 的交错扫描。
        let targets = parse(
            args,
            &RM,
            Scan {
                stop_at_operand: true,
                abort_on_unknown: false,
            },
        );
        // 递归或语法上的批量即可：不展开目标，帮助 / 交互选项不取消已知的危险证据。
        let rule = (enabled(&parsed.flags, &["-r", "-R", "--recursive"], &[])
            || targets.operands.len() >= 2
            || targets.operands.iter().any(|word| word.glob))
        .then_some("remove-critical-path");
        return settle(rule, &parsed);
    }
    if matches!(name, "dd" | "mount" | "umount" | "mkfs") || mkfs_variant(name) {
        // 只豁免单独、明确的查询形式。
        if args.len() == 1
            && !args[0].dynamic
            && (args[0].value == "--help" || args[0].value == "--version")
        {
            return Hit::No;
        }
        if name == "mount" && args.is_empty() {
            return Hit::No;
        }
        return Hit::Rule("system-storage");
    }
    if name == "rsync" {
        let parsed = parse(args, &RSYNC, COMMAND_SCAN);
        let has = |list: &[&str]| {
            parsed
                .flags
                .iter()
                .any(|flag| list.contains(&flag.as_str()))
        };
        let rule = (!has(&["-n", "--dry-run", "--help", "--version"]) && has(&RSYNC_DELETE))
            .then_some("rsync-delete");
        return settle(rule, &parsed);
    }
    Hit::No
}

/// Node `matchDangerousCommand`：逐条简单命令剥掉包装、按命令名匹配，第一个命中即返回。
pub fn dangerous(command: &str, dialect: Dialect) -> Danger {
    if command.encode_utf16().count() > MAX_LENGTH {
        return Danger::Unsupported;
    }
    let windows = dialect == Dialect::GitBash;
    let analysis = crate::parse::analyze(command);
    // 解析失败没有可依赖的定位；局部的不支持结构不丢弃其他独立的命令。
    if analysis.has_parse_errors {
        return Danger::Unsupported;
    }
    let mut unsupported = analysis.has_unsupported_syntax;
    for invocation_ in &analysis.commands {
        let words: Vec<Word> = invocation_.words.iter().map(Word::from_text).collect();
        let Some(words) = unwrap(&words, windows).filter(|words| !words.is_empty()) else {
            unsupported = true;
            continue;
        };
        let hit = basename(&words[0], windows)
            .map_or(Hit::Unknown, |name| invocation(&name, &words[1..], dialect));
        if let Hit::Rule(rule) = hit {
            return Danger::Matched(format!("safety.bash.{rule}"));
        }
        if hit == Hit::Unknown || words.iter().any(|word| word.dynamic) {
            unsupported = true;
        }
    }
    if unsupported {
        Danger::Unsupported
    } else {
        Danger::NotMatched
    }
}

/// 这次调用要不要按 guarded 先询问一次：模式是 guarded、工具是 Bash、命令命中规则（Node
/// `!planEnabled && mode === "guarded" && userApprovalRule`；plan 开时判决用的模式是 plan，不会到这里）。
pub fn applies(mode: PermissionMode, tool: &str, input: &Value, style: PathStyle) -> bool {
    if mode != PermissionMode::Guarded || tool != "Bash" {
        return false;
    }
    let command = input
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default();
    matches!(dangerous(command, Dialect::of(style)), Danger::Matched(_))
}

#[cfg(test)]
#[path = "guarded_tests.rs"]
mod tests;
