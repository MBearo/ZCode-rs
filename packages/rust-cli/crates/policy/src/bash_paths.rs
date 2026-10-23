//! Bash 子命令的路径检查（shell-permissions.md §4.5）。
//!
//! 文件类命令（`mkdir`、`touch`、`rm`、`cat`、`grep` 等）与输出重定向的目标路径按操作分读、写：
//! 命中 deny 规则拒绝；读工作目录以外、写工作目录以外、以及非 acceptEdits 模式下写工作目录里的
//! 路径都要询问，询问文本逐字固定（`texts.rs`）。复合命令里有 `cd` 又有写入时一律询问。

use rust_cli_kernel_api::permission::{PermissionMode, RuleBehavior};

use crate::check::{AskKind, Check, Inputs};
use crate::files::{Access, path_rule};
use crate::parse::Invocation;
use crate::readonly::strip_wrappers;
use crate::texts;

/// 命令的动作说明与操作类型。
fn command_kind(name: &str) -> Option<(&'static str, Access)> {
    use Access::{Read, Write};
    Some(match name {
        "cd" => ("change directories to", Read),
        "ls" => ("list files in", Read),
        "find" => ("search files in", Read),
        "mkdir" => ("create directories in", Write),
        "touch" => ("create or modify files in", Write),
        "rm" => ("remove files from", Write),
        "rmdir" => ("remove directories from", Write),
        "mv" => ("move files to/from", Write),
        "cp" => ("copy files to/from", Write),
        "cat" => ("concatenate files from", Read),
        "head" => ("read the beginning of files from", Read),
        "tail" => ("read the end of files from", Read),
        "sort" => ("sort contents of files from", Read),
        "uniq" => ("filter duplicate lines from files in", Read),
        "wc" => ("count lines/words/bytes in files from", Read),
        "cut" => ("extract columns from files in", Read),
        "paste" => ("merge files from", Read),
        "column" => ("format files from", Read),
        "file" => ("examine file types in", Read),
        "stat" => ("read file stats from", Read),
        "diff" | "comm" | "cmp" => ("compare files from", Read),
        "strings" => ("extract strings from files in", Read),
        "nl" => ("number lines in files from", Read),
        "tac" => ("read files (reversed) from", Read),
        "grep" | "egrep" | "fgrep" | "rg" => ("search for patterns in files from", Read),
        "sed" => ("edit files in", Write),
        "jq" => ("process JSON from files in", Read),
        "sha256sum" => ("compute SHA-256 checksums for files in", Read),
        "sha1sum" => ("compute SHA-1 checksums for files in", Read),
        "md5sum" => ("compute MD5 checksums for files in", Read),
        "tee" => ("write to files in", Write),
        _ => return None,
    })
}

/// 带值的选项：值不是路径。
fn takes_value(name: &str, flag: &str) -> bool {
    let flags: &[&str] = match name {
        "head" | "tail" => &["-n", "-c"],
        "cut" => &["-d", "-f", "-c", "-b"],
        "sort" => &["-k", "-t", "-S", "-T"],
        "grep" | "egrep" | "fgrep" => &["-e", "-A", "-B", "-C", "-m"],
        "rg" => &["-e", "-A", "-B", "-C", "-m", "-g", "-t", "-T"],
        "uniq" => &["-f", "-s"],
        "touch" => &["-d", "-t", "-r"],
        "mkdir" => &["-m"],
        "sed" => &["-e"],
        "paste" => &["-d"],
        "jq" => &["--arg", "--argjson"],
        _ => &[],
    };
    flags.contains(&flag)
}

/// 第一个位置参数不是路径（模式、脚本、程序）。
fn leading_operand(name: &str, argv: &[String]) -> bool {
    match name {
        "grep" | "egrep" | "fgrep" | "rg" | "sed" => {
            !argv.iter().any(|arg| arg == "-e" || arg == "-f")
        }
        "jq" => true,
        _ => false,
    }
}

/// find 的路径参数：`--` 之后的词都是路径；`-H` / `-L` / `-P`
/// 跳过；别的以 `-` 开头的词标记“已经有选项”，是带路径的谓词（`-newer`、`-samefile`、`-path` 等与 `/^-newer[acmBt][acmtB]$/`）
/// 时下一个词是路径；第一个选项之前的普通词是路径。没有路径时是 `.`。
///
/// 工作线 F53：以前遇到 `(` / `!` 就停、谓词的值当作路径（`find . -name /etc` 询问），改成上面的规则。bfs 允许路径写在表达式
/// 后面（`find \( -name x \) /etc`、`find -name x /etc`、`find . -f /etc` 都会搜 /etc），这条规则不校验它们：用户已知晓，
/// 决定保留（2026-10-22，shell-permissions.md §4.5.1 第 2 条）。
fn find_operands(args: &[String]) -> Vec<String> {
    const PATH_VALUED: [&str; 11] = [
        "-newer",
        "-anewer",
        "-cnewer",
        "-mnewer",
        "-samefile",
        "-path",
        "-wholename",
        "-ilname",
        "-lname",
        "-ipath",
        "-iwholename",
    ];
    let newer_xy = |word: &str| {
        word.strip_prefix("-newer").is_some_and(|rest| {
            let rest = rest.as_bytes();
            rest.len() == 2 && b"acmBt".contains(&rest[0]) && b"acmtB".contains(&rest[1])
        })
    };
    let mut paths = Vec::new();
    let mut option_seen = false;
    let mut literal = false;
    let mut index = 0;
    while index < args.len() {
        let word = args[index].as_str();
        index += 1;
        if word.is_empty() {
            continue;
        }
        if literal {
            paths.push(word.to_owned());
            continue;
        }
        if word == "--" {
            literal = true;
            continue;
        }
        if word.starts_with('-') {
            if matches!(word, "-H" | "-L" | "-P") {
                continue;
            }
            option_seen = true;
            if (PATH_VALUED.contains(&word) || newer_xy(word))
                && let Some(value) = args.get(index).filter(|value| !value.is_empty())
            {
                paths.push(value.clone());
                index += 1;
            }
            continue;
        }
        if !option_seen {
            paths.push(word.to_owned());
        }
    }
    if paths.is_empty() {
        paths.push(".".to_owned());
    }
    paths
}

/// 位置参数里的路径；`cd` 没有参数时是 home，`ls` / `find` 没有参数时是 cwd。
fn operands(name: &str, argv: &[String]) -> Vec<String> {
    if name == "find" {
        return find_operands(argv.get(1..).unwrap_or_default());
    }
    let mut paths = Vec::new();
    let mut literal = false;
    let mut skip = false;
    let mut leading = leading_operand(name, argv);
    for arg in argv.iter().skip(1) {
        if skip {
            skip = false;
            continue;
        }
        if !literal && arg == "--" {
            literal = true;
            continue;
        }
        if !literal && arg.starts_with('-') && arg.len() > 1 {
            skip = takes_value(name, arg);
            continue;
        }
        if leading {
            leading = false;
            continue;
        }
        paths.push(arg.clone());
    }
    if paths.is_empty() {
        match name {
            "cd" => paths.push("~".to_owned()),
            "ls" => paths.push(".".to_owned()),
            _ => {}
        }
    }
    paths
}

/// 一个目标路径的检查。`subject` 用在询问文本里（`touch in '<path>'`、`Output redirection to …`）。
struct Target {
    command: String,
    access: Access,
    action: &'static str,
    path: String,
    subject: String,
}

fn verdict(target: &Target, inputs: &Inputs<'_>) -> Option<(Check, bool)> {
    let paths = inputs.paths;
    let dirs = texts::directories(&paths.root_list());
    let inside = paths.in_working_dirs(&target.path);
    let allowed = match target.access {
        Access::Read => inside || paths.internal_readable(&target.path),
        Access::Write => {
            (inside && inputs.mode == PermissionMode::AcceptEdits)
                || path_rule(Access::Write, &target.path, RuleBehavior::Allow, inputs)
        }
    };
    if allowed {
        return None;
    }
    let message = match (target.access, inside) {
        (Access::Write, true) => texts::write_inside(&target.subject, &dirs),
        (Access::Write, false) => texts::write_outside(&target.subject, &dirs),
        (Access::Read, _) => {
            texts::read_outside(&target.command, &target.path, target.action, &dirs)
        }
    };
    // 工作目录里的写入可以被这条命令的 allow 规则盖过。
    Some((
        Check::ask(message),
        target.access == Access::Write && inside,
    ))
}

/// 路径检查的结果：`Passthrough` 表示没有意见；`overridable` 为 true 时 allow 规则可以盖过询问。
pub struct PathVerdict {
    pub check: Check,
    pub overridable: bool,
    /// 复合命令里有 cd 又有写入：整条命令直接按这个询问。
    pub cd_with_write: bool,
}

fn redirect_targets(invocation: &Invocation, inputs: &Inputs<'_>) -> Vec<Target> {
    invocation
        .redirects
        .iter()
        .filter(|redirect| matches!(redirect.operator.as_str(), ">" | ">>" | ">|" | "&>" | "&>>"))
        .filter(|redirect| redirect.target != "/dev/null" && !redirect.target.starts_with('&'))
        .map(|redirect| {
            let path = inputs.paths.resolve(&redirect.target);
            Target {
                command: "Output redirection".to_owned(),
                access: Access::Write,
                action: "write to files in",
                subject: format!("Output redirection to '{path}'"),
                path,
            }
        })
        .collect()
}

/// 检查一个子命令。`compound_has_cd`：同一条命令里另有 `cd`。
pub fn check(invocation: &Invocation, compound_has_cd: bool, inputs: &Inputs<'_>) -> PathVerdict {
    let argv = strip_wrappers(&invocation.argv);
    let name = argv.first().map(String::as_str).unwrap_or_default();
    let mut targets = redirect_targets(invocation, inputs);
    if let Some((action, mut access)) = command_kind(name) {
        if name == "sed"
            && !argv
                .iter()
                .any(|arg| arg == "-i" || arg.starts_with("-i") || arg == "--in-place")
        {
            access = Access::Read;
        }
        if access == Access::Write && invocation.has_dynamic_words {
            return asked(texts::runtime_target(name), false);
        }
        if matches!(name, "mv" | "cp") && argv.iter().skip(1).any(|arg| arg.starts_with('-')) {
            return asked(
                format!(
                    "{name} with flags requires manual approval to ensure path safety. For security, ZCode cannot automatically validate {name} commands that use flags, as some flags like --target-directory=PATH can bypass path validation."
                ),
                false,
            );
        }
        let operands = operands(name, argv);
        let last = operands.len().saturating_sub(1);
        for (index, operand) in operands.iter().enumerate() {
            let path = inputs.paths.resolve(operand);
            // cp 只写最后一个参数，前面的是读。
            let access = if name == "cp" && index < last {
                Access::Read
            } else {
                access
            };
            targets.push(Target {
                command: name.to_owned(),
                access,
                action,
                subject: format!("{name} in '{path}'"),
                path,
            });
        }
    }
    if compound_has_cd && targets.iter().any(|target| target.access == Access::Write) {
        return PathVerdict {
            check: Check::Ask {
                message: texts::CD_WITH_WRITE.to_owned(),
                kind: AskKind::Other,
                suggestions: Vec::new(),
            },
            overridable: false,
            cd_with_write: true,
        };
    }
    for target in &targets {
        if path_rule(target.access, &target.path, RuleBehavior::Deny, inputs) {
            let preposition = if target.access == Access::Read {
                "from"
            } else {
                "to"
            };
            return PathVerdict {
                check: Check::deny(texts::path_rule_denied(
                    &target.command,
                    preposition,
                    &target.path,
                )),
                overridable: false,
                cd_with_write: false,
            };
        }
    }
    let mut first_overridable = None;
    for target in &targets {
        match verdict(target, inputs) {
            Some((check, true)) => {
                first_overridable.get_or_insert(check);
            }
            Some((check, false)) => {
                return PathVerdict {
                    check,
                    overridable: false,
                    cd_with_write: false,
                };
            }
            None => {}
        }
    }
    match first_overridable {
        Some(check) => PathVerdict {
            check,
            overridable: true,
            cd_with_write: false,
        },
        None => PathVerdict {
            check: Check::Passthrough {
                reason: None,
                suggestions: Vec::new(),
            },
            overridable: false,
            cd_with_write: false,
        },
    }
}

fn asked(message: String, overridable: bool) -> PathVerdict {
    PathVerdict {
        check: Check::Ask {
            message,
            kind: AskKind::Other,
            suggestions: Vec::new(),
        },
        overridable,
        cd_with_write: false,
    }
}
