//! 路径：解析、工作目录判断与路径规则（shell-permissions.md §4.4）。纯字符串运算，不碰文件系统。
//!
//! 路径规则（`Read(...)`、`Edit(...)`）按 gitignore 风格：`//x` 是绝对路径，`~/x` 在 home 下，
//! `/x` 相对规则的根（工作区根），其余相对根且不带 `/` 时在任意层级匹配文件名。`**` 跨目录，
//! `*`、`?` 不跨目录；匹配到目录时它下面的一切都算匹配。

use regex::{Regex, RegexBuilder};
use rust_cli_kernel_api::env_files::PathStyle;
use rust_cli_kernel_api::env_path::git_bash_spelling;
use rust_cli_kernel_api::permission::RuleBehavior;
use rust_cli_kernel_api::plan::PlanFiles;

use crate::consts::DRIVE_ROOT_LEN;

/// `//c/x` 去掉 `//` 之后的盘符与它后面的 `/`（`c/`）的字节数。
const SLASHED_DRIVE_LEN: usize = 2;

/// 一次判决里解析路径所需的事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathContext {
    /// 当前 shell cwd：相对路径以它为基准。
    pub cwd: String,
    /// 本会话的工作目录：工作区根加 `additionalDirectories`。
    pub roots: Vec<String>,
    pub home: Option<String>,
    /// 执行环境的路径风格（`path_facts`；答不出时按 POSIX）。路径规则是 kernel-api `env_path`。
    pub style: PathStyle,
    /// 不必询问就能读的目录（命令输出、工具结果）。
    pub internal: Vec<String>,
    /// 本会话的计划文件（tools-more.md §2.5）；执行环境答不出路径风格时为 None。
    pub plans: Option<PlanFiles>,
}

impl PathContext {
    /// 用户写的路径换成规范的绝对路径：`~` 展开到 home，相对路径接在 cwd 后面。
    pub fn resolve(&self, path: &str) -> String {
        self.style.resolve(&self.cwd, path, self.home.as_deref())
    }

    /// 文件工具的路径参数：去首尾空白、NFC、只认 `~/`。判定必须与
    /// 工具取同一条规则（tools-files.md §3.2）。
    pub fn resolve_input(&self, path: &str) -> String {
        self.style
            .resolve_user_path(&self.cwd, path, self.home.as_deref())
    }

    pub fn in_working_dirs(&self, path: &str) -> bool {
        self.roots
            .iter()
            .any(|root| self.style.is_within(path, root))
    }

    pub fn internal_readable(&self, path: &str) -> bool {
        self.internal
            .iter()
            .any(|root| self.style.is_within(path, root))
    }

    /// 本会话的计划文件：读写都放行（tools-more.md §2.5）。
    pub fn is_plan_file(&self, path: &str) -> bool {
        self.plans
            .as_ref()
            .is_some_and(|plans| plans.contains(path))
    }

    /// 消息里列出的工作目录。
    pub fn root_list(&self) -> Vec<String> {
        self.roots.clone()
    }
}

/// glob 换成正则：`**` 跨目录，`*` / `?` 不跨目录；不分大小写（F57）。
fn glob_regex(glob: &str) -> Option<Regex> {
    let mut out = String::from("^");
    let mut chars = glob.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' if chars.peek() == Some(&'*') => {
                chars.next();
                if chars.peek() == Some(&'/') {
                    chars.next();
                    out.push_str("(?:.*/)?");
                } else {
                    out.push_str(".*");
                }
            }
            '*' => out.push_str("[^/]*"),
            '?' => out.push_str("[^/]"),
            other => out.push_str(&regex::escape(&other.to_string())),
        }
    }
    out.push('$');
    RegexBuilder::new(&out).case_insensitive(true).build().ok()
}

/// 规则的根与相对根的 glob。根是本机写法
/// （`/`、工作区根、home 或 Windows 的 `C:\`），glob 不以 `/` 开头。
pub(crate) fn anchor(pattern: &str, root: &str, context: &PathContext) -> (String, String) {
    let windows = context.style == PathStyle::Windows;
    // Windows：`~\x`、`\x`（`\!`、`\#` 是 gitignore 的转义，除外）先把 `\` 全换成 `/`。
    let escaped = matches!(pattern.as_bytes().get(1), Some(b'!' | b'#'));
    let pattern =
        if windows && (pattern.starts_with(r"~\") || (pattern.starts_with('\\') && !escaped)) {
            pattern.replace('\\', "/")
        } else {
            pattern.to_owned()
        };
    if let Some(rest) = pattern.strip_prefix("//") {
        // `//c/x`：Windows 上根是 `C:\`（`/^\/[a-z]\//i`）；其余（含 `//C:/x`）的根是 `/`。
        let bytes = rest.as_bytes();
        if windows
            && bytes.len() >= SLASHED_DRIVE_LEN
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b'/'
        {
            let letter = char::from(bytes[0]).to_ascii_uppercase();
            return (format!(r"{letter}:\"), rest[SLASHED_DRIVE_LEN..].to_owned());
        }
        return ("/".to_owned(), rest.to_owned());
    }
    let bytes = pattern.as_bytes();
    if windows
        && bytes.len() >= DRIVE_ROOT_LEN
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
    {
        // `C:/x`、`C:\x`：根是 `C:\`，其余的 `\` 换成 `/`。
        let letter = char::from(bytes[0]).to_ascii_uppercase();
        return (
            format!(r"{letter}:\"),
            pattern[DRIVE_ROOT_LEN..].replace('\\', "/"),
        );
    }
    if let (Some(rest), Some(home)) = (pattern.strip_prefix("~/"), &context.home) {
        return (home.clone(), rest.to_owned());
    }
    if let Some(rest) = pattern.strip_prefix('/') {
        return (root.to_owned(), rest.to_owned());
    }
    if let Some(rest) = pattern.strip_prefix("./") {
        return (root.to_owned(), rest.to_owned());
    }
    if pattern.trim_end_matches('/').contains('/') {
        return (root.to_owned(), pattern);
    }
    (root.to_owned(), format!("**/{pattern}"))
}

/// 匹配用的大小写折叠：小写，`ı` → `i`，`ſ` → `s`。
fn fold_case(text: &str) -> String {
    text.to_lowercase()
        .replace('\u{131}', "i")
        .replace('\u{17f}', "s")
}

/// 路径规则是否覆盖 `path`（已规范化的绝对路径）。`root` 是规则的根（工作区根）。
///
/// 先算目标相对规则根的
/// 路径（不在根下就不命中），再拿相对路径匹配 glob；匹配到某个上级目录时，目录下的一切都算匹配。
///
/// 缺陷修复（工作线 F57，shell-permissions.md §4.2）：
/// - 以前整条路径用区分大小写的正则匹配，deny `Read(.env)` 拦不住 `.ENV`；现在 glob
///   部分不分大小写（所有平台）。根部分：Windows 上 deny 与 ask 规则折叠大小写，allow 与其他平台照原样。
/// - 以前 Windows 上只把 `\` 换成 `/`（`C:/x`），`//c/x/**`、`C:/x/**` 都不命中；现在目标、根与 home 先换成 Git Bash
///   写法（`/c/x`），模式按 [`anchor`] 换根。
pub fn rule_matches(
    pattern: &str,
    path: &str,
    root: &str,
    context: &PathContext,
    behavior: RuleBehavior,
) -> bool {
    let windows = context.style == PathStyle::Windows;
    let fold = windows && behavior != RuleBehavior::Allow;
    let spell = |text: &str| {
        let text = if windows {
            git_bash_spelling(text)
        } else {
            text.to_owned()
        };
        if fold { fold_case(&text) } else { text }
    };
    let (base, glob) = anchor(pattern, root, context);
    let (base, target) = (spell(&base), spell(path));
    let Some(relative) = target
        .strip_prefix(base.trim_end_matches('/'))
        .and_then(|rest| rest.strip_prefix('/'))
        .filter(|rest| !rest.is_empty())
    else {
        return false;
    };
    let glob = glob.trim_end_matches('/');
    let Some(regex) = glob_regex(glob) else {
        return false;
    };
    // 修复（工作线 F85）：`x/**` 也匹配 `x` 目录本身（对照实现按 gitignore 的 `x` 匹配）。以前只匹配它下面的，
    // `Read(//etc/**)` 下 `cd /etc`、`ls /etc` 只按工作目录外询问。
    if let Some(directory) = glob
        .strip_suffix("/**")
        .filter(|directory| directory.contains(|c| c != '/'))
        && glob_regex(directory).is_some_and(|regex| regex.is_match(relative))
    {
        return true;
    }
    let mut candidate = relative;
    loop {
        if regex.is_match(candidate) {
            return true;
        }
        match candidate.rfind('/') {
            None => return false,
            Some(at) => candidate = &candidate[..at],
        }
    }
}

#[cfg(test)]
#[path = "paths_tests.rs"]
mod tests;
