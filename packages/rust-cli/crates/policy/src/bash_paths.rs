//! Bash 子命令的路径检查（shell-permissions.md §4.5）。
//!
//! 文件类命令（`mkdir`、`touch`、`rm`、`cat`、`grep` 等）、输出重定向与输入重定向的目标路径按操作分读、写：
//! 命中 deny 规则拒绝；读工作目录以外、写工作目录以外、以及非 acceptEdits 模式下写工作目录里的
//! 路径都要询问，询问文本逐字固定（`texts.rs`）。复合命令里有 `cd` 又有写入时一律询问；有 `cd` 又按相对路径读、
//! 配置了 Read deny 规则时询问（§4.5.7）。

use rust_cli_kernel_api::permission::{PermissionMode, RuleBehavior};

use crate::check::{AskKind, Check, Inputs};
use crate::files::{Access, path_rule};
use crate::parse::Invocation;
use crate::removal::{basename, strip_runners};
use crate::{removal, texts};

/// 命令名：去掉目录只对 `rm`、`rmdir`、`tee` 做（同对照实现；`/bin/cat` 不算 cat）。
pub(crate) fn command_name(word: &str) -> &str {
    let base = basename(word);
    if matches!(base, "rm" | "rmdir" | "tee") {
        base
    } else {
        word
    }
}

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
pub(crate) fn operands(name: &str, argv: &[String]) -> Vec<String> {
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

/// 输出重定向的目标命中路径 deny 规则（[`check`] 先查重定向的目标，它拒绝时拒绝就来自重定向）。管道里这种拒绝
/// 保留路径的文字（shell-permissions.md §4.5.5，工作线 F80）。
pub fn redirect_denied(invocation: &Invocation, inputs: &Inputs<'_>) -> bool {
    redirect_targets(invocation, inputs)
        .iter()
        .any(|target| path_rule(target.access, &target.path, RuleBehavior::Deny, inputs))
}

/// 路径命令的动作与读写；`sed` 没有 `-i` 时是读。不是路径命令时 None。
fn kind_of(name: &str, argv: &[String]) -> Option<(&'static str, Access)> {
    let (action, access) = command_kind(name)?;
    let read_only_sed = name == "sed"
        && !argv
            .iter()
            .any(|arg| arg == "-i" || arg.starts_with("-i") || arg == "--in-place");
    Some((action, if read_only_sed { Access::Read } else { access }))
}

/// 这条命令读的路径（原文）：读命令的路径参数、`cp` / `mv` 的源、`sed` 的参数，加上输入重定向的目标
/// （§4.5.7 里不能解析的 cd 之后按 deny 规则的目录查它们）。
pub(crate) fn read_paths(invocation: &Invocation) -> Vec<String> {
    let argv = strip_runners(&invocation.argv);
    let mut paths = Vec::new();
    if let Some(first) = argv.first() {
        let name = command_name(first);
        if let Some((_, access)) = kind_of(name, argv) {
            let found = operands(name, argv);
            match (access, name) {
                (Access::Read, _) | (_, "sed") => paths.extend(found),
                (_, "cp" | "mv") if found.len() >= 2 => {
                    paths.extend(found[..found.len() - 1].iter().cloned());
                }
                _ => {}
            }
        }
    }
    paths.extend(input_targets(invocation));
    paths
        .into_iter()
        .filter(|path| !path.contains(['$', '`']) && !standard_stream(path))
        .collect()
}

/// 检查一个子命令。`compound_has_cd`：同一组命令里另有 `cd`（§4.5.7）。
///
/// 修复（工作线 F85）：以前先问运行时才知道的目标、`mv` / `cp` 的选项与“cd 加写入”，再查路径 deny 规则，
/// `cp -r a /etc/x`（`Edit(//etc/**)`）只询问，guarded 与 yolo 下执行；现在照对照实现先查 deny 规则。输入重定向的目标
/// 以前不查（`cat < /etc/hosts` 放行），现在最先查拒绝、最后才问。
pub fn check(invocation: &Invocation, compound_has_cd: bool, inputs: &Inputs<'_>) -> PathVerdict {
    let argv = strip_runners(&invocation.argv);
    let name = argv.first().map_or("", |first| command_name(first));
    let input = input_verdict(invocation, compound_has_cd, inputs);
    if let Some(denied @ Check::Deny { .. }) = &input {
        return verdict_of(denied.clone(), false);
    }
    // 危险删除（§4.5.3，修复线 F68）：路径的 deny 规则之后、其余路径询问（含“cd 加写入”与运行时才知道的目标）之前；
    // allow 规则盖不过它。
    let removal = removal::check(invocation, compound_has_cd, inputs).map(|check| PathVerdict {
        check,
        overridable: false,
        cd_with_write: false,
    });
    let mut targets = redirect_targets(invocation, inputs);
    let kind = kind_of(name, argv);
    let mut operand_list = Vec::new();
    if let Some((action, access)) = kind {
        operand_list = operands(name, argv);
        let last = operand_list.len().saturating_sub(1);
        for (index, operand) in operand_list.iter().enumerate() {
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
    for target in &targets {
        if path_rule(target.access, &target.path, RuleBehavior::Deny, inputs) {
            let preposition = if target.access == Access::Read {
                "from"
            } else {
                "to"
            };
            return verdict_of(
                Check::deny(texts::path_rule_denied(
                    &target.command,
                    preposition,
                    &target.path,
                )),
                false,
            );
        }
    }
    if let Some(removal) = removal {
        return removal;
    }
    if let Some((_, access)) = kind {
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
        // 工作线 F85（§4.5.7）：有 cd、按相对路径读、配置了 Read deny 规则时询问（cd 的目标不能静态确定）。
        if compound_has_cd
            && access == Access::Read
            && name != "cd"
            && read_deny_configured(inputs)
            && operand_list.iter().any(|operand| {
                !inputs
                    .paths
                    .style
                    .is_absolute(&expand_home(operand, inputs))
            })
        {
            return asked(format!("{name} {}", texts::CD_RELATIVE_READ), false);
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
    match (first_overridable, input) {
        // 输入重定向的询问排在其余路径询问之后（§4.5.8）。
        (Some(check), _) => PathVerdict {
            check,
            overridable: true,
            cd_with_write: false,
        },
        (None, Some(check)) => verdict_of(check, false),
        (None, None) => PathVerdict {
            check: Check::Passthrough {
                reason: None,
                suggestions: Vec::new(),
            },
            overridable: false,
            cd_with_write: false,
        },
    }
}

fn verdict_of(check: Check, overridable: bool) -> PathVerdict {
    PathVerdict {
        check,
        overridable,
        cd_with_write: false,
    }
}

/// 配置了 Read 的 deny 规则（整个工具或带路径的）。
pub(crate) fn read_deny_configured(inputs: &Inputs<'_>) -> bool {
    inputs
        .rules
        .whole_tool(Access::Read.rule_tool(), RuleBehavior::Deny)
        .is_some()
        || !inputs
            .rules
            .contents(Access::Read.rule_tool(), RuleBehavior::Deny)
            .is_empty()
}

/// `~` / `~/…` 展开成 home（其余原样）。
fn expand_home(path: &str, inputs: &Inputs<'_>) -> String {
    match (inputs.paths.home.as_deref(), path.strip_prefix('~')) {
        (Some(home), Some(rest)) if rest.is_empty() || rest.starts_with('/') => {
            format!("{home}{rest}")
        }
        _ => path.to_owned(),
    }
}

/// 不查的输入：标准流与 `/dev/fd/N`。
fn standard_stream(path: &str) -> bool {
    matches!(
        path,
        "/dev/null" | "/dev/stdin" | "/dev/stdout" | "/dev/stderr" | "/dev/tty"
    ) || path
        .strip_prefix("/dev/fd/")
        .is_some_and(|fd| !fd.is_empty() && fd.bytes().all(|b| b.is_ascii_digit()))
        || path
            .strip_prefix("/proc/self/fd/")
            .is_some_and(|fd| matches!(fd, "0" | "1" | "2"))
}

/// 输入重定向的目标：`<`，以及目标不是数字或 `-` 的 `<&`。
fn input_targets(invocation: &Invocation) -> Vec<String> {
    invocation
        .redirects
        .iter()
        .filter(|redirect| match redirect.operator.as_str() {
            "<" => true,
            "<&" => {
                let descriptor = !redirect.target.is_empty()
                    && redirect.target.bytes().all(|b| b.is_ascii_digit());
                redirect.target != "-" && !descriptor
            }
            _ => false,
        })
        .map(|redirect| redirect.target.clone())
        .collect()
}

/// 输入重定向（§4.5.8，工作线 F85）：拒绝或询问；没有意见时 None。
fn input_verdict(
    invocation: &Invocation,
    compound_has_cd: bool,
    inputs: &Inputs<'_>,
) -> Option<Check> {
    let paths = inputs.paths;
    let mut asked: Option<Check> = None;
    for target in input_targets(invocation) {
        if standard_stream(&target) {
            continue;
        }
        let glob = target.contains(['*', '?', '[']);
        let relative = !paths.style.is_absolute(&target) && !target.starts_with('~');
        if glob || (compound_has_cd && relative) {
            // 有 cd 时相对路径不按当前目录查（它指向哪里不确定），只查与当前目录无关的写法。
            let expanded = expand_home(&target, inputs);
            let mut candidates = Vec::new();
            if expanded != target {
                candidates.push(expanded);
            }
            if !target.starts_with('~') && (!compound_has_cd || !relative) {
                candidates.push(paths.resolve(&target));
            }
            if let Some(path) = candidates
                .into_iter()
                .find(|path| path_rule(Access::Read, path, RuleBehavior::Deny, inputs))
            {
                return Some(Check::deny(texts::path_rule_denied(
                    "Input redirection",
                    "from",
                    &path,
                )));
            }
            asked.get_or_insert_with(|| Check::ask(texts::input_unresolved(&target)));
            continue;
        }
        let path = paths.resolve(&target);
        if path_rule(Access::Read, &path, RuleBehavior::Deny, inputs) {
            return Some(Check::deny(texts::path_rule_denied(
                "Input redirection",
                "from",
                &path,
            )));
        }
        if paths.in_working_dirs(&path) || paths.internal_readable(&path) {
            continue;
        }
        let dirs = texts::directories(&paths.root_list());
        asked.get_or_insert_with(|| Check::ask(texts::input_outside(&path, &dirs)));
    }
    asked
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
