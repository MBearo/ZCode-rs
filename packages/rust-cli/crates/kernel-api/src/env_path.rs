//! 执行环境路径（[`EnvPath`](crate::env::EnvPath)）的字符串规则，按 [`PathStyle`] 区分
//! （docs/specs/rust-cli/tools-files.md §3.1）。纯字符串运算，不做 IO。
//!
//! 唯一来源：tools、runtime、context、policy、agents、ext、memory、cli 都调用这里，不各写一份。
//! 语义是 Node `path.posix` / `path.win32`（路径展开、取目录与比较用的键都建立在它们上面）。
//! 约定：结果去掉末尾的分隔符（根除外），即
//! `path.resolve` 的输出形式。

use rust_cli_base::js::string::{is_space, trim};

use crate::env_files::PathStyle;

/// 根前缀的种类。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Root {
    /// 相对路径。
    None,
    /// `/`（POSIX）或 `\`（Windows 当前盘的根）。
    Slash,
    /// `C:\`：盘符根。
    Drive(char),
    /// `C:`：盘符相对路径。
    DriveRelative(char),
    /// `\\server\share\`。
    Unc(String, String),
}

impl Root {
    fn is_absolute(&self) -> bool {
        matches!(self, Self::Slash | Self::Drive(_) | Self::Unc(..))
    }

    fn render(&self, style: PathStyle) -> String {
        let sep = style.separator();
        match self {
            Self::None => String::new(),
            Self::Slash => sep.to_string(),
            Self::Drive(letter) => format!("{letter}:{sep}"),
            Self::DriveRelative(letter) => format!("{letter}:"),
            Self::Unc(server, share) => format!("{sep}{sep}{server}{sep}{share}{sep}"),
        }
    }
}

impl PathStyle {
    /// 按执行环境的 platform：`win32` 为 Windows，其余为 POSIX。
    pub fn of_platform(platform: &str) -> Self {
        if platform == "win32" {
            Self::Windows
        } else {
            Self::Posix
        }
    }

    /// 只有一个绝对路径时按它的写法推断：盘符或 `\` 开头为 Windows。
    pub fn infer(path: &str) -> Self {
        if drive_letter(path).is_some() || path.starts_with('\\') {
            Self::Windows
        } else {
            Self::Posix
        }
    }

    pub fn separator(self) -> char {
        match self {
            Self::Posix => '/',
            Self::Windows => '\\',
        }
    }

    pub fn is_separator(self, c: char) -> bool {
        c == '/' || (self == Self::Windows && c == '\\')
    }

    /// 根与其余部分（Node `path.win32` 的根识别：UNC 要有服务器名与共享名）。
    fn split_root(self, path: &str) -> (Root, &str) {
        let bytes = path.as_bytes();
        let sep = |at: usize| {
            bytes
                .get(at)
                .is_some_and(|b| self.is_separator(char::from(*b)))
        };
        match self {
            Self::Posix if sep(0) => (Root::Slash, &path[1..]),
            Self::Posix => (Root::None, path),
            Self::Windows if sep(0) && sep(1) => match unc(self, &path[2..]) {
                Some((server, share, rest)) => (Root::Unc(server, share), rest),
                None => (Root::Slash, &path[1..]),
            },
            Self::Windows if sep(0) => (Root::Slash, &path[1..]),
            Self::Windows => match drive_letter(path) {
                Some(letter) if sep(2) => (Root::Drive(letter), &path[3..]),
                Some(letter) => (Root::DriveRelative(letter), &path[2..]),
                None => (Root::None, path),
            },
        }
    }

    /// Node `path.isAbsolute`：Windows 上 `\x`、`/x`、`C:\x`、UNC 都算，`C:x` 不算。
    pub fn is_absolute(self, path: &str) -> bool {
        self.split_root(path).0.is_absolute()
    }

    /// 去掉 `.`、折叠 `..`、合并分隔符、去掉末尾分隔符；Windows 统一用 `\`。绝对路径的 `..` 停在根，
    /// 相对路径保留开头的 `..`；相对路径为空时是 `.`（Node `path.normalize`）。
    pub fn normalize(self, path: &str) -> String {
        let (root, rest) = self.split_root(path);
        let mut parts: Vec<&str> = Vec::new();
        for part in rest.split(|c| self.is_separator(c)) {
            match part {
                "" | "." => {}
                ".." if parts.last().is_some_and(|last| *last != "..") => {
                    parts.pop();
                }
                ".." if root.is_absolute() => {}
                other => parts.push(other),
            }
        }
        let head = root.render(self);
        if parts.is_empty() && !root.is_absolute() {
            return format!("{head}.");
        }
        format!("{head}{}", parts.join(&self.separator().to_string()))
    }

    /// `base` 后面依次接上 `parts`，再规范化（Node `path.join` 加去尾）。
    pub fn join(self, base: &str, parts: &[&str]) -> String {
        let mut joined = base.to_owned();
        for part in parts {
            joined.push(self.separator());
            joined.push_str(part);
        }
        self.normalize(&joined)
    }

    /// 用户路径是否要靠 home 才能解析（`~`、`~/x`，Windows 另认 `~\x`）。
    pub fn needs_home(self, input: &str) -> bool {
        input == "~" || self.tilde_rest(input).is_some()
    }

    fn tilde_rest(self, input: &str) -> Option<&str> {
        input
            .strip_prefix("~/")
            .or_else(|| input.strip_prefix("~\\").filter(|_| self == Self::Windows))
    }

    /// 用户写的路径换成绝对路径：`~` / `~/x` 按 home（没有 home 时当普通
    /// 相对路径）；Windows 上 `/c/x` 换成 `C:\x`；绝对路径规范化；其余接在 `base` 后面。
    /// 不 trim、不做 NFC（旧实现都不做，登记为差异）。
    pub fn resolve(self, base: &str, input: &str, home: Option<&str>) -> String {
        if let Some(home) = home {
            if input == "~" {
                return self.normalize(home);
            }
            if let Some(rest) = self.tilde_rest(input) {
                return self.join(home, &[rest]);
            }
        }
        let converted = self.posix_drive(input);
        let input = converted.as_deref().unwrap_or(input);
        match self.split_root(input) {
            (root, _) if root.is_absolute() => self.normalize(input),
            (Root::DriveRelative(letter), rest) => {
                // 盘符相对：同盘接在基准后面，否则接在那个盘的根后面（Node `path.win32.resolve`）。
                let same_drive =
                    drive_letter(base).is_some_and(|own| own.eq_ignore_ascii_case(&letter));
                if same_drive {
                    self.join(base, &[rest])
                } else {
                    self.join(&Root::Drive(letter).render(self), &[rest])
                }
            }
            _ => self.join(base, &[input]),
        }
    }

    /// Windows 上把 `/c/x` 换成 `C:\x`。
    fn posix_drive(self, input: &str) -> Option<String> {
        if self != Self::Windows {
            return None;
        }
        let bytes = input.as_bytes();
        let shaped = bytes.len() >= 3
            && bytes[0] == b'/'
            && bytes[1].is_ascii_alphabetic()
            && bytes[2] == b'/';
        shaped.then(|| {
            let letter = char::from(bytes[1]).to_ascii_uppercase();
            format!("{letter}:{}", input[2..].replace('/', "\\"))
        })
    }

    /// 上一层目录；已经是根（或 `.`）时为 None。相对路径只有一段时是 `.`。
    pub fn parent(self, path: &str) -> Option<String> {
        let normalized = self.normalize(path);
        let (root, rest) = self.split_root(&normalized);
        if rest.is_empty() || rest == "." {
            return None;
        }
        Some(match rest.rfind(|c| self.is_separator(c)) {
            Some(at) => format!("{}{}", root.render(self), &rest[..at]),
            None if root == Root::None => ".".to_owned(),
            None => root.render(self),
        })
    }

    /// Node `path.dirname`（对规范化后的路径）：根的上一层是它自己。
    pub fn dirname(self, path: &str) -> String {
        self.parent(path).unwrap_or_else(|| self.normalize(path))
    }

    /// 最后一段；根与 `.` 没有名字（空串）。
    pub fn file_name(self, path: &str) -> String {
        let normalized = self.normalize(path);
        let (_, rest) = self.split_root(&normalized);
        match rest.rsplit(|c| self.is_separator(c)).next() {
            Some(".") | None => String::new(),
            Some(name) => name.to_owned(),
        }
    }

    /// 从根的下一层到 `dir` 的每一层，由上往下；根本身不在内。
    pub fn ancestors(self, dir: &str) -> Vec<String> {
        let mut dirs = Vec::new();
        let mut current = self.normalize(dir);
        while let Some(parent) = self.parent(&current) {
            dirs.push(std::mem::replace(&mut current, parent));
        }
        dirs.reverse();
        dirs
    }

    /// 比较用的键：Windows 上不分大小写。
    pub fn key(self, path: &str) -> String {
        let normalized = self.normalize(path);
        match self {
            Self::Posix => normalized,
            Self::Windows => normalized.to_lowercase(),
        }
    }

    pub fn same(self, left: &str, right: &str) -> bool {
        self.key(left) == self.key(right)
    }

    /// `path` 在 `dir` 里（含相等）。
    pub fn is_within(self, path: &str, dir: &str) -> bool {
        let (path, dir) = (self.key(path), self.key(dir));
        let sep = self.separator();
        path == dir || path.starts_with(&format!("{}{sep}", dir.trim_end_matches(sep)))
    }

    /// `path` 在 `dir` 里时的剩余部分（分隔符按风格）；相等时为空串，不在里面时为 None。
    pub fn relative(self, dir: &str, path: &str) -> Option<String> {
        if !self.is_within(path, dir) {
            return None;
        }
        let dir = self.normalize(dir);
        let path = self.normalize(path);
        let rest = path.get(dir.len()..).unwrap_or_default();
        Some(rest.trim_start_matches(|c| self.is_separator(c)).to_owned())
    }
}

/// `C:` 开头时的盘符。
fn drive_letter(path: &str) -> Option<char> {
    let bytes = path.as_bytes();
    (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
        .then(|| char::from(bytes[0]))
}

/// `\\` 之后的 `server\share[\rest]`：服务器名与共享名都不能为空（Node `path.win32`）。其余部分不带
/// 开头的分隔符（根已经以分隔符结尾）。
fn unc(style: PathStyle, after: &str) -> Option<(String, String, &str)> {
    let is_sep = |c: char| style.is_separator(c);
    if after.is_empty() || after.starts_with(is_sep) {
        return None;
    }
    let server_end = after.find(is_sep)?;
    let tail = after[server_end..].trim_start_matches(is_sep);
    let share_end = tail.find(is_sep).unwrap_or(tail.len());
    if share_end == 0 {
        return None;
    }
    Some((
        after[..server_end].to_owned(),
        tail[..share_end].to_owned(),
        tail[share_end..].trim_start_matches(is_sep),
    ))
}

/// 网络路径（`\\server`、`//server`，不分风格）：不从这类位置读规则文件，也不在信任前
/// 按仓库内容去读（Windows 上会泄露凭据）。
pub fn is_unc(path: &str) -> bool {
    path.starts_with("\\\\") || (path.starts_with("//") && !path.starts_with("///"))
}

/// Windows 路径的 Git Bash 写法（工作线 F57）：`\\` 开头的把 `\` 换成 `/`（`\\server\x` → `//server/x`）；
/// `C:\x` / `C:/x` 写成 `/c/x`（盘符转小写）；其余把 `\` 换成 `/`。没有分隔符的 `C:`、`C:x` 只换 `\`。
///
/// 用处：Bash 的 prelude 与 cwd 文件（env-local），Windows 上权限规则的匹配（policy 先把目标与根换成这种写法）。
/// 对绝对路径与 Node `windowsPathToGitBashPath` 的结果相同。
pub fn git_bash_spelling(path: &str) -> String {
    if path.starts_with(r"\\") {
        return path.replace('\\', "/");
    }
    let bytes = path.as_bytes();
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
    {
        let drive = char::from(bytes[0]).to_ascii_lowercase();
        return format!("/{drive}{}", path[2..].replace('\\', "/"));
    }
    path.replace('\\', "/")
}

/// Node `path.extname` 的小写形式：最后一段里最后一个 `.` 起的部分；以 `.` 开头且没有别的 `.` 时为空。
pub fn extension(path: &str) -> String {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    match name.rfind('.') {
        Some(0) | None => String::new(),
        Some(at) => name[at..].to_lowercase(),
    }
}

/// Unicode NFC（JS `String.prototype.normalize("NFC")`）：工具的路径参数解析后都做（[`PathStyle::resolve_user_path`]）。
pub fn nfc(text: &str) -> String {
    icu_normalizer::ComposingNormalizerBorrowed::new_nfc()
        .normalize(text)
        .into_owned()
}

/// 工具的路径参数解析后以空白结尾、再解析一次会变成别的路径。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnstableUserPath;

impl PathStyle {
    /// 工具的路径参数是否要靠 home 才能解析：去掉首尾空白后是 `~` 或以 `~/` 开头（Windows
    /// 也不认 `~\`）。
    pub fn user_path_needs_home(self, input: &str) -> bool {
        let trimmed = trim(input);
        trimmed == "~" || trimmed.starts_with("~/")
    }

    /// 工具的路径参数换成绝对路径（
    /// docs/specs/rust-cli/tools-files.md §3.2）：先去首尾空白（JS `trim`），空串是基准目录；`~`、`~/x` 按 home
    /// （没有 home 时当普通相对路径；Windows 上的 `~\x` 也是普通相对路径）；Windows 上 `/c/x` 换成 `C:\x`；绝对
    /// 路径规范化，其余接在 `base` 后面；结果做 NFC。与 [`Self::resolve`] 的区别只在 trim、NFC 与 `~\x`。
    pub fn resolve_user_path(self, base: &str, input: &str, home: Option<&str>) -> String {
        self.user_path(base, input, home).0
    }

    /// 同 [`Self::resolve_user_path`]，但结果再解析一次会变时拒绝：Read / Write / Edit / NotebookEdit 用它。
    ///
    /// 只有 Node 形式的结果以空白结尾时才会变（例如 `/tmp/x /.` 规范化成 `/tmp/x `，再 trim 就成了 `/tmp/x`）。
    /// Node 的 `normalize` / `join` 保留输入末尾的分隔符，所以 `/tmp/x /` 是稳定的；本模块的结果总去掉末尾
    /// 分隔符（写成 `/tmp/x `），判断时按 Node 的形式。
    pub fn resolve_user_path_stable(
        self,
        base: &str,
        input: &str,
        home: Option<&str>,
    ) -> Result<String, UnstableUserPath> {
        let (resolved, node_trailing_separator) = self.user_path(base, input, home);
        if !node_trailing_separator && resolved.ends_with(is_space) {
            return Err(UnstableUserPath);
        }
        Ok(resolved)
    }

    /// 解析结果，与 Node 的结果是否以分隔符结尾（`normalize` / `join` 的两支保留输入末尾的分隔符，`resolve` 去掉）。
    fn user_path(self, base: &str, input: &str, home: Option<&str>) -> (String, bool) {
        let trimmed = trim(input);
        if trimmed.is_empty() {
            return (nfc(&self.normalize(base)), false);
        }
        let ends_with_separator = |text: &str| text.ends_with(|c| self.is_separator(c));
        if let Some(home) = home {
            if trimmed == "~" {
                return (nfc(&self.normalize(home)), false);
            }
            if let Some(rest) = trimmed.strip_prefix("~/") {
                return (nfc(&self.join(home, &[rest])), ends_with_separator(rest));
            }
        }
        let converted = self.posix_drive(trimmed);
        let text = converted.as_deref().unwrap_or(trimmed);
        if self.is_absolute(text) {
            return (nfc(&self.normalize(text)), ends_with_separator(text));
        }
        // 相对路径：没有 home 时 `~` 不展开（`resolve` 只在给了 home 时展开）。
        (nfc(&self.resolve(base, text, None)), false)
    }
}

#[cfg(test)]
#[path = "env_path_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "env_path_user_tests.rs"]
mod user_tests;
