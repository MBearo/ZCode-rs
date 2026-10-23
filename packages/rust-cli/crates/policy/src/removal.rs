//! 危险删除（shell-permissions.md §4.5.3）。
//!
//! `rm` / `rmdir` 删除关键目录（根、根下的一级目录、home）、工作目录或它的上级，以及静态定不下目标的删除都要询问；
//! 询问的性质是 bypass 也问、allow 规则盖不过（[`AskKind::Immune`]）。目标先按变量为空时的样子看，再看命令替换，
//! 最后按字面路径看。
//!
//! 修复线 F68（审计 P2）：以前没有这项检查，yolo / bypass 下 `rm -rf <工作区>` 直接执行，工作区被删。

use std::sync::OnceLock;

use regex::{Regex, RegexBuilder};
use rust_cli_kernel_api::env_files::PathStyle;

use crate::check::{AskKind, Check, Inputs};
use crate::parse::{Invocation, WordText};
use crate::readonly::strip_wrappers;
use crate::texts::{self, Removal};

/// 根下的一级目录名：变量为空时 `$X/<名字>` 就是它。
const TOP_LEVEL: &str = r"bin|boot|dev|etc|home|lib|lib32|lib64|libx32|media|mnt|opt|proc|root|run|sbin|srv|sys|tmp|usr|var|snap|nix|lost\+found|private|cores|Applications|Library|System|Users|Volumes|Windows|ProgramData|cygdrive";

/// 平常有值的变量：守卫写法拦不住，建议写字面路径；也当作不会为空（一级目录那条规则不对它们询问）。
const NORMALLY_SET: [&str; 25] = [
    "HOME",
    "USERPROFILE",
    "HOMEPATH",
    "HOMEDRIVE",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
    "APPDATA",
    "LOCALAPPDATA",
    "SYSTEMDRIVE",
    "SYSTEMROOT",
    "WINDIR",
    "PROGRAMFILES",
    "PROGRAMW6432",
    "PROGRAMDATA",
    "ALLUSERSPROFILE",
    "PUBLIC",
    "HOMESHARE",
    "XDG_RUNTIME_DIR",
    "PWD",
    "OLDPWD",
    "TMPDIR",
    "TMP",
    "TEMP",
];

/// 询问里命令文字的最大长度（字符）。
const INVOCATION_LIMIT: usize = 120;

const NAME: &str = r"[A-Za-z_][A-Za-z0-9_]*";

struct Patterns {
    /// `$NAME/`、`${NAME}/`（可带 `:-` 缺省值与引号）后面接结尾、glob、`$`、`/` 或引号：变量为空时是根。
    root: Regex,
    /// `$NAME/<一级目录名>`：变量为空时是那个一级目录。
    top: Regex,
    /// 目标里的每个变量（守卫写法逐个替换）。
    variable: Regex,
    /// 末尾的 `/*`。
    trailing_glob: Regex,
}

fn patterns() -> Option<&'static Patterns> {
    static PATTERNS: OnceLock<Option<Patterns>> = OnceLock::new();
    PATTERNS
        .get_or_init(|| {
            let default = format!(r#"(?::?-(?:""|''|"?\$\{{?({NAME})\}}?"?)?)?"#);
            Some(Patterns {
                root: Regex::new(&format!(
                    r#"^["']*\$(?:\{{{NAME}{default}\}}|{NAME})["']*\\?/(?:[*?\[{{]|\$|/|["']|$)"#
                ))
                .ok()?,
                top: RegexBuilder::new(&format!(
                    r#"^["']*\$(?:\{{({NAME}){default}\}}|({NAME}))["']*\\?/({TOP_LEVEL})(?:[\\/]+\*+)*/?["']*$"#
                ))
                .case_insensitive(true)
                .build()
                .ok()?,
                variable: Regex::new(&format!(
                    r#"\$(?:\{{({NAME}|[0-9]+|[@*!])(?::?-(?:["']{{2}}|"?\$\{{?({NAME})\}}?"?)?)?\}}|({NAME}|[0-9]+|[@*!]))"#
                ))
                .ok()?,
                trailing_glob: Regex::new(r"([\\/]\*+)+[\\/]*$").ok()?,
            })
        })
        .as_ref()
}

/// 去掉执行别的命令的包装（`command`、`time`、`nohup`、`timeout N`、`nice`、`stdbuf`、`env`），得到真正执行的命令。
fn strip_runners(argv: &[String]) -> &[String] {
    let mut words = strip_wrappers(argv);
    loop {
        let name = words.first().map(|word| basename(word));
        let skip_dashes = |at: usize, words: &[String]| {
            if words.get(at).is_some_and(|word| word == "--") {
                at + 1
            } else {
                at
            }
        };
        let next = match name {
            Some("time" | "nohup") => skip_dashes(1, words),
            Some("timeout") => {
                let mut at = 1;
                while let Some(word) = words.get(at).filter(|word| word.starts_with('-')) {
                    at += if matches!(word.as_str(), "-k" | "-s") {
                        2
                    } else {
                        1
                    };
                }
                let duration = words.get(at).is_some_and(|word| {
                    let digits = word.trim_end_matches(['s', 'm', 'h', 'd']);
                    !digits.is_empty()
                        && digits
                            .split('.')
                            .all(|part| part.bytes().all(|b| b.is_ascii_digit()))
                });
                if !duration {
                    return words;
                }
                at + 1
            }
            Some("nice") => match words.get(1).map(String::as_str) {
                Some("-n") => skip_dashes(3, words),
                Some(flag) if flag.starts_with('-') && flag.len() > 1 => skip_dashes(2, words),
                _ => skip_dashes(1, words),
            },
            Some("stdbuf") => {
                let mut at = 1;
                while words.get(at).is_some_and(|word| word.starts_with('-')) {
                    at += 1;
                }
                at
            }
            Some("env") => {
                let mut at = 1;
                while let Some(word) = words.get(at) {
                    if word == "-u" {
                        at += 2;
                    } else if word.starts_with('-') || word.contains('=') {
                        at += 1;
                    } else {
                        break;
                    }
                }
                at
            }
            _ => return words,
        };
        if next >= words.len() {
            return words;
        }
        words = strip_wrappers(&words[next..]);
    }
}

fn basename(word: &str) -> &str {
    word.rsplit(['/', '\\']).next().unwrap_or(word)
}

/// 删除的目标：`--` 之后或第一个不以 `-` 开头的参数之后全是目标，单独的 `-` 也是。`(值, 原文)`。
fn targets<'a>(args: &'a [String], raw: &'a [WordText]) -> Vec<(&'a str, &'a str)> {
    let mut found = Vec::new();
    let mut literal = false;
    for (value, raw) in args.iter().zip(raw) {
        if literal {
            found.push((value.as_str(), raw.text.as_str()));
        } else if value == "--" {
            literal = true;
        } else if value == "-" || !value.starts_with('-') {
            found.push((value.as_str(), raw.text.as_str()));
            literal = true;
        }
    }
    found
}

/// 检查一条简单命令；不是 rm / rmdir 或没有危险时为 None。`compound_has_cd`：同一条命令里另有 `cd`。
pub(crate) fn check(
    invocation: &Invocation,
    compound_has_cd: bool,
    inputs: &Inputs<'_>,
) -> Option<Check> {
    let argv = strip_runners(&invocation.argv);
    // 原文与值按位置对齐（管道的 `time` 关键字让原文多一个词）。
    let raw = invocation
        .words
        .get(invocation.words.len().checked_sub(argv.len())?..)?;
    let command = basename(argv.first()?);
    if command != "rm" && command != "rmdir" {
        return None;
    }
    let args = &argv[1..];
    let found = targets(args, &raw[1..]);
    let removal = Removing {
        command,
        args,
        raw,
        compound_has_cd,
        inputs,
    };
    let message = found
        .iter()
        .find_map(|(_, raw)| removal.variable(raw))
        .or_else(|| {
            found
                .iter()
                .find_map(|(value, raw)| removal.target(value, raw))
        })?;
    Some(Check::Ask {
        message,
        kind: AskKind::Immune,
        suggestions: Vec::new(),
    })
}

struct Removing<'a> {
    command: &'a str,
    args: &'a [String],
    raw: &'a [WordText],
    compound_has_cd: bool,
    inputs: &'a Inputs<'a>,
}

impl Removing<'_> {
    fn style(&self) -> PathStyle {
        self.inputs.paths.style
    }

    /// 变量为空时目标会变成根或根下的一级目录。
    fn variable(&self, raw: &str) -> Option<String> {
        let patterns = patterns()?;
        // `$HOME`（不带花括号）按 home 解析，交给字面路径的规则。
        if raw.starts_with('-') || raw.starts_with('\'') || bare_home(raw).is_some() {
            return None;
        }
        let collapsed = collapse_slashes(raw);
        let root_child = patterns.top.captures(&collapsed).and_then(|found| {
            let name = found.get(1).or_else(|| found.get(3))?.as_str();
            let fallback = found.get(2).map(|value| value.as_str());
            (may_be_unset(name) && fallback.is_none_or(may_be_unset))
                .then(|| found.get(4).map(|child| child.as_str().to_owned()))
                .flatten()
        });
        if root_child.is_none() && !patterns.root.is_match(raw) {
            return None;
        }
        Some(self.variable_message(raw, root_child.as_deref(), patterns))
    }

    fn variable_message(&self, raw: &str, root_child: Option<&str>, patterns: &Patterns) -> String {
        let command = self.command;
        let mut names: Vec<String> = Vec::new();
        // （名字、原来的写法、守卫写法）。
        let mut guards: Vec<(String, String, String)> = Vec::new();
        let mut guarded = String::new();
        let mut last = 0;
        for found in patterns.variable.captures_iter(raw) {
            let Some(whole) = found.get(0) else { continue };
            let name = found
                .get(1)
                .or_else(|| found.get(3))
                .map_or("", |name| name.as_str());
            names.push(name.to_owned());
            let mut guard = format!("${{{name}:?}}");
            if let Some(fallback) = found.get(2) {
                names.push(fallback.as_str().to_owned());
                guard = format!("${{{name}:-${{{}:?}}}}", fallback.as_str());
            }
            if quote_at(raw, whole.start()).is_none() {
                guard = format!("\"{guard}\"");
            }
            guarded.push_str(&raw[last..whole.start()]);
            guarded.push_str(&guard);
            last = whole.end();
            guards.push((name.to_owned(), whole.as_str().to_owned(), guard));
        }
        guarded.push_str(&raw[last..]);
        let mut unique: Vec<String> = Vec::new();
        for name in if names.is_empty() {
            vec!["VAR".to_owned()]
        } else {
            names
        } {
            if !unique.contains(&name) {
                unique.push(name);
            }
        }
        let listed = |filter: &dyn Fn(&str) -> bool| {
            unique
                .iter()
                .filter(|name| filter(name))
                .map(|name| format!("${name}"))
                .collect::<Vec<_>>()
                .join(" or ")
        };
        let names = listed(&|_| true);
        let normally_set = listed(&|name| NORMALLY_SET.contains(&name.to_uppercase().as_str()));
        let positional = |name: &str| {
            name.bytes().all(|b| b.is_ascii_digit()) || matches!(name, "@" | "*" | "!")
        };
        let positionals = listed(&|name| positional(name));
        let advice = if let Some(child) = root_child {
            format!(
                "rewrite it as `{guarded}`, which makes the shell stop with an error instead of removing /{child} when {names} is unset or empty (it is not set in the environment this command inherits), or use a literal absolute path"
            )
        } else if !normally_set.is_empty() {
            format!(
                "use a literal absolute path: {normally_set} is normally set, so a guard on it would not stop this {command}"
            )
        } else if !positionals.is_empty() {
            let rewrites = guards
                .iter()
                .filter(|(name, _, _)| !positional(name))
                .map(|(_, written, guard)| format!("`{written}` as `{guard}`"))
                .collect::<Vec<_>>()
                .join(" and ");
            let rewrite = if rewrites.is_empty() {
                String::new()
            } else {
                format!(" and rewrite its {rewrites}")
            };
            format!(
                "bind {positionals}{rewrite} (in an inline `sh -c` script, single-quote the script so this shell does not expand the positional) or use a literal absolute path"
            )
        } else {
            let rest = guards.iter().fold(guarded.clone(), |text, (_, _, guard)| {
                text.replacen(guard, "", 1)
            });
            if guards.is_empty() || rest.contains('$') {
                "use a literal absolute path: this check cannot verify part of this target"
                    .to_owned()
            } else {
                format!(
                    "rewrite it as `{guarded}`, which makes the shell stop with an error instead of running {command} when {names} is unset or empty, or use a literal absolute path"
                )
            }
        };
        texts::dangerous_variable_removal(
            command,
            &self.invocation(raw),
            raw,
            &names,
            root_child,
            &advice,
        )
    }

    /// 询问里的命令：这条 rm 的词用一个空格连起来，太长时截断。
    fn invocation(&self, target: &str) -> String {
        let words: Vec<String> = self
            .raw
            .iter()
            .map(|word| word.text.split_whitespace().collect::<Vec<_>>().join(" "))
            .collect();
        let whole = words.join(" ");
        if whole.chars().count() <= INVOCATION_LIMIT {
            return whole;
        }
        let head: String = whole.chars().take(INVOCATION_LIMIT).collect();
        let upto = words
            .iter()
            .position(|word| word == target)
            .map_or(words.len(), |at| at + 1);
        if words[..upto].join(" ").chars().count() <= INVOCATION_LIMIT {
            format!("{head}\u{2026}")
        } else {
            format!("{head}\u{2026} {target}")
        }
    }

    /// 命令替换与字面路径。
    fn target(&self, value: &str, raw: &str) -> Option<String> {
        if let Some(rest) = substitution_suffix(raw) {
            // 目标整个是命令替换：`rm -r` 时询问。
            if rest.is_empty() {
                return (self.command == "rm" && self.recursive())
                    .then(texts::dangerous_substitution_removal);
            }
            // 目标以命令替换结尾：替换为空时就是前面的路径。
            if !rest.contains(['$', '`', '"', '\'']) {
                return self.literal(rest);
            }
            return None;
        }
        if let Some(rest) = bare_home(raw) {
            let home = self.inputs.paths.home.as_deref()?;
            return self.literal(&format!("{home}{rest}"));
        }
        if raw.contains(['$', '`']) {
            return None;
        }
        self.literal(value)
    }

    /// 选项里有 `-r` / `-R` / `--recursive`（`--` 之前）。
    fn recursive(&self) -> bool {
        self.args.iter().take_while(|arg| *arg != "--").any(|arg| {
            arg.starts_with("--r")
                || (arg.starts_with('-')
                    && !arg.starts_with("--")
                    && arg[1..].bytes().all(|b| b.is_ascii_alphabetic())
                    && arg[1..].contains(['r', 'R']))
        })
    }

    /// 字面路径的目标。
    fn literal(&self, original: &str) -> Option<String> {
        let style = self.style();
        let paths = self.inputs.paths;
        let command = self.command;
        if style == PathStyle::Windows
            && !original.is_empty()
            && original.bytes().all(|b| b == b'\\')
        {
            return Some(texts::dangerous_backslash_removal(command, original));
        }
        let expanded = expand_tilde(original, paths.home.as_deref());
        let absolute = style.is_absolute(&expanded);
        let target = if absolute {
            expanded.clone()
        } else {
            style.resolve(&paths.cwd, &expanded, None)
        };
        let base = strip_trailing_globs(&target, style)?;
        let globbed = base != target;
        let ask = |why| Some(texts::dangerous_removal(command, &target, why));
        if self.compound_has_cd && globbed && !absolute && ends_with_glob(&target) {
            return ask(Removal::ChangesDirectories);
        }
        let rest = if absolute {
            base.clone()
        } else {
            let separator = style.separator();
            let prefix = if paths.cwd.ends_with(['/', '\\']) {
                paths.cwd.clone()
            } else {
                format!("{}{separator}", paths.cwd)
            };
            match base.strip_prefix(&prefix) {
                Some(rest) => rest.to_owned(),
                None if base == paths.cwd => String::new(),
                None => base.clone(),
            }
        };
        let rmdir_parents = command == "rmdir"
            && self.args.iter().any(|arg| {
                arg.starts_with("--p")
                    || (arg.starts_with('-') && !arg.starts_with("--") && arg.contains('p'))
            });
        if globbed
            && (dotdot_after_name(&expanded)
                || expanded.starts_with('~')
                || expanded.starts_with("//")
                || expanded.starts_with("\\\\")
                || (!absolute && has_dotdot(&expanded) && ends_with_glob(&target))
                || (rmdir_parents && ends_with_glob(&target))
                || (!absolute && ends_with_star_slash(&expanded) && ends_with_glob(&target)))
        {
            return ask(Removal::Unresolved);
        }
        if !globbed || !has_glob(&rest) {
            if critical(&base, paths.home.as_deref(), style) {
                return ask(Removal::Critical);
            }
            let workspace = std::iter::once(&paths.cwd)
                .chain(paths.roots.iter().filter(|_| globbed))
                .any(|dir| same_or_inside(dir, &base, style));
            if workspace {
                return ask(Removal::Workspace);
            }
        }
        if globbed && ends_with_glob(&target) {
            let removed = segments(&target).saturating_sub(segments(&base));
            let globs = rest
                .split(['/', '\\'])
                .filter(|segment| has_glob(segment))
                .count();
            if removed + globs > 1 {
                return ask(Removal::Traverses);
            }
        }
        None
    }
}

/// 变量是不是可能为空：平常有值的变量当作有值（不看环境，D18）。
fn may_be_unset(name: &str) -> bool {
    !NORMALLY_SET.contains(&name.to_uppercase().as_str())
}

/// `$HOME` 开头（单独或在双引号里，不带花括号）时后面的部分；后面还有展开或引号时为 None。
fn bare_home(raw: &str) -> Option<&str> {
    let quoted = raw
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .and_then(|inner| inner.strip_prefix("$HOME"));
    let rest = quoted
        .or_else(|| raw.strip_prefix("\"$HOME\""))
        .or_else(|| raw.strip_prefix("$HOME"))?;
    ((rest.is_empty() || rest.starts_with('/')) && !rest.contains(['$', '`', '"', '\'']))
        .then_some(rest)
}

/// `~` / `~/…` 换成 home。
fn expand_tilde(path: &str, home: Option<&str>) -> String {
    match (home, path.strip_prefix('~')) {
        (Some(home), Some(rest)) if rest.is_empty() || rest.starts_with('/') => {
            format!("{home}{rest}")
        }
        _ => path.to_owned(),
    }
}

/// 反复去掉末尾的 `/*`；全去掉时是根。
fn strip_trailing_globs(path: &str, style: PathStyle) -> Option<String> {
    let patterns = patterns()?;
    let mut current = path.to_owned();
    loop {
        let stripped = patterns.trailing_glob.replace(&current, "");
        let stripped = if stripped.is_empty() { "/" } else { &stripped };
        if stripped == current {
            return Some(current);
        }
        current = if stripped.contains(['/', '\\']) {
            style.normalize(stripped)
        } else {
            stripped.to_owned()
        };
    }
}

fn ends_with_glob(path: &str) -> bool {
    path.ends_with("/*") || path.ends_with("\\*")
}

fn ends_with_star_slash(path: &str) -> bool {
    path.trim_end_matches(['/', '\\']).len() < path.len()
        && path.trim_end_matches(['/', '\\']).ends_with('*')
}

fn has_glob(text: &str) -> bool {
    text.contains(['*', '?', '['])
}

/// 路径里有 `..` 段。
fn has_dotdot(path: &str) -> bool {
    path.split(['/', '\\']).any(|segment| segment == "..")
}

/// `..` 出现在一个普通段之后。
fn dotdot_after_name(path: &str) -> bool {
    let mut named = false;
    for segment in path.split(['/', '\\']) {
        match segment {
            "" | "." => {}
            ".." if named => return true,
            ".." => {}
            _ => named = true,
        }
    }
    false
}

/// 非空、不是 `.` 的段数。
fn segments(path: &str) -> usize {
    path.split(['/', '\\'])
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .count()
}

/// 比较用的写法：`\` 换成 `/`、不分大小写；macOS 的 `/private/var/`、`/private/tmp` 当作 `/var/`、`/tmp`。
fn comparable(path: &str, style: PathStyle) -> String {
    let normalized = style.normalize(path).replace('\\', "/").to_lowercase();
    if let Some(rest) = normalized.strip_prefix("/private/var/") {
        return format!("/var/{rest}");
    }
    match normalized.strip_prefix("/private/tmp") {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("/tmp{rest}"),
        _ => normalized,
    }
}

/// `dir` 是 `target` 本身或在它下面（删 `target` 会删掉 `dir`）。
fn same_or_inside(dir: &str, target: &str, style: PathStyle) -> bool {
    let dir = comparable(dir, style);
    let target = comparable(target, style);
    let target = target.trim_end_matches('/');
    dir == target || target.is_empty() || dir.starts_with(&format!("{target}/"))
}

/// 关键目录：根、根下的一级目录、home、Windows 的盘符根与一级目录。
fn critical(path: &str, home: Option<&str>, style: PathStyle) -> bool {
    let slashed = |text: &str| {
        let text = text.replace('\\', "/");
        if cfg!(target_os = "macos") {
            for name in ["etc", "var", "tmp", "home"] {
                let prefix = format!("/private/{name}");
                if text.len() >= prefix.len()
                    && text[..prefix.len()].eq_ignore_ascii_case(&prefix)
                    && (text.len() == prefix.len() || text[prefix.len()..].starts_with('/'))
                {
                    return format!("/{name}{}", &text[prefix.len()..]);
                }
            }
        }
        text
    };
    let mut variants = vec![slashed(path)];
    let without_dots: Vec<&str> = path
        .split('/')
        .enumerate()
        .filter(|(index, segment)| *segment != "." || *index == 0)
        .map(|(_, segment)| segment)
        .collect();
    variants.push(slashed(&without_dots.join("/")));
    let home = home.map(|home| slashed(home).trim_end_matches('/').to_lowercase());
    variants.iter().any(|variant| {
        if variant == "*" || variant.ends_with("/*") {
            return true;
        }
        let trimmed = if variant == "/" {
            variant.as_str()
        } else {
            variant.strip_suffix('/').unwrap_or(variant)
        };
        let drive = trimmed.as_bytes();
        let drive_root = drive.len() >= 2
            && drive[0].is_ascii_alphabetic()
            && drive[1] == b':'
            && (drive.len() == 2 || (drive.len() == 3 && drive[2] == b'/'));
        let drive_top = drive.len() > 3
            && drive[0].is_ascii_alphabetic()
            && drive[1] == b':'
            && drive[2] == b'/'
            && !trimmed[3..].contains('/');
        trimmed == "/"
            || drive_root
            || drive_top
            || home.as_deref() == Some(trimmed.to_lowercase().as_str())
            || (style != PathStyle::Windows
                && trimmed.starts_with('/')
                && trimmed.len() > 1
                && !trimmed[1..].contains('/'))
    })
}

/// 目标原文里开头的命令替换（可带一层双引号，后面可接 `/`、`*`、`.`）拿掉之后剩下的前缀；以替换结尾时返回
/// 前面的部分，整个是替换时返回空串，不是这两种时为 None。
fn substitution_suffix(raw: &str) -> Option<&str> {
    let unquoted = raw
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .unwrap_or(raw);
    if let Some(end) = substitutions_end(unquoted, 0)
        && end == unquoted.len()
    {
        return Some("");
    }
    // 以替换结尾：从每个 `$(` / 反引号起试一次，前面至少有一个字符。
    raw.char_indices()
        .filter(|(at, c)| *at > 0 && (*c == '`' || raw[*at..].starts_with("$(")))
        .find(|(at, _)| substitutions_end(raw, *at) == Some(raw.len()))
        .map(|(at, _)| &raw[..at])
}

/// 从 `start` 起一个或几个命令替换（各自后面可接 `/`、`*`、`.`）的结尾；不是时为 None。
fn substitutions_end(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut at = start;
    let mut any = false;
    while at < bytes.len() {
        if bytes[at] == b'`' {
            at += 1 + text[at + 1..].find('`')? + 1;
        } else if text[at..].starts_with("$(") {
            let mut depth = 0usize;
            let mut end = None;
            for (offset, byte) in bytes[at..].iter().enumerate() {
                match byte {
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(at + offset + 1);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            at = end?;
        } else {
            return None;
        }
        any = true;
        while at < bytes.len() && matches!(bytes[at], b'/' | b'*' | b'.') {
            at += 1;
        }
    }
    any.then_some(at)
}

/// 合并多余的 `/` 与 `/./`（一级目录那条规则先这样规范化）。
fn collapse_slashes(raw: &str) -> String {
    let mut text = raw.replace("/./", "/");
    while text.contains("/./") {
        text = text.replace("/./", "/");
    }
    while text.contains("//") {
        text = text.replace("//", "/");
    }
    text
}

/// `offset` 处是不是在引号里（返回引号）。
fn quote_at(text: &str, offset: usize) -> Option<char> {
    let mut quote = None;
    let mut chars = text[..offset].chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (None, '\\') => {
                chars.next();
            }
            (None, '"' | '\'') => quote = Some(c),
            (Some('"'), '\\') => {
                chars.next();
            }
            (Some(open), c) if c == open => quote = None,
            _ => {}
        }
    }
    quote
}
