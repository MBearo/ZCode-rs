// 复制自 rust/crates/kit/src/paths.rs（feat/rs 79b6691c55）。
//! 用户目录（docs/specs/rust-u0-foundation.md §3.2，缺陷 B4）。

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

/// 取 home 的平台规则（Node `os.homedir()`，即 libuv `uv_os_homedir`；docs/specs/rust-cli/config.md §2.3，工作线 F57）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HomeRule {
    /// `HOME`，没有时查系统用户数据库（`getpwuid_r`）。
    Unix,
    /// `USERPROFILE`，没有时用系统 API（`GetUserProfileDirectoryW`，由入口给出）。
    Windows,
}

impl HomeRule {
    /// 本机的规则。
    pub(crate) const CURRENT: Self = if cfg!(windows) {
        Self::Windows
    } else {
        Self::Unix
    };

    fn variable(self) -> &'static str {
        match self {
            Self::Unix => rust_cli_base::names::env::HOME,
            Self::Windows => rust_cli_base::names::env::USERPROFILE,
        }
    }
}

/// 当前用户的 home 目录，照 Node `os.homedir()`：Unix 是 `HOME` → 系统用户数据库（`getpwuid_r`）；Windows 是
/// `USERPROFILE` → 系统 API（`windows_profile`，入口传 `std::env::home_dir()`，即 `GetUserProfileDirectoryW`；
/// 其他平台不调用它）。
///
/// 空值按未设置处理；都拿不到时返回错误，绝不返回空路径。环境变量从 `lookup` 读取：
/// 只有入口 crate `cli` 读进程环境，它把启动时捕获的快照传进来（docs/specs/rust-cli/README.md §4）。
/// 本机存储、`~` 展开、凭据密钥与执行环境的 home 都用这一个结果（config.md §2.3）。
pub fn home_dir_with(
    lookup: impl Fn(&str) -> Option<OsString>,
    windows_profile: impl FnOnce() -> Option<PathBuf>,
) -> io::Result<PathBuf> {
    home_for(HomeRule::CURRENT, lookup, windows_profile)
}

/// [`home_dir_with`] 的规则本身；平台由参数给出，macOS 上也能测 Windows 的规则。
pub(crate) fn home_for(
    rule: HomeRule,
    lookup: impl Fn(&str) -> Option<OsString>,
    windows_profile: impl FnOnce() -> Option<PathBuf>,
) -> io::Result<PathBuf> {
    // B4 缺陷修复：旧实现在 HOME 为空、USERPROFILE 未设置时返回空 PathBuf，之后的
    // `home.join(".zcode")` 变成相对路径，数据落到当前工作目录下。现在空值视为未设置，
    // 再退回系统；仍拿不到就报错，由调用方让启动失败。
    // F57 缺陷修复：以前所有平台都是 `HOME` → `USERPROFILE`。Windows 上设了 `HOME`（Git Bash、MSYS2 常设）时，
    // 会话库、配置根与 `~` 落在 `HOME` 下，Node 落在 `USERPROFILE` 下；凭据密钥另按 Node 取，两个 home 不一致。
    // 现在照 Node：每个平台只看自己的变量，再退回系统。
    if let Some(value) = lookup(rule.variable()).filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(value));
    }
    match rule {
        HomeRule::Unix => user_database_home(),
        HomeRule::Windows => windows_profile()
            .filter(|path| !path.as_os_str().is_empty())
            .ok_or_else(|| unavailable(rule, None)),
    }
}

/// 当前用户名（Node `os.userInfo().username`）：Unix 查系统用户数据库（有效用户），Windows 取 `USERNAME`
/// （libuv 的 GetUserNameW 与它一致，Host 同样处理）。拿不到时为 `unknown`（Node 凭据密钥的回退值）。
pub fn user_name_with(lookup: impl Fn(&str) -> Option<OsString>) -> String {
    if cfg!(windows) {
        return lookup(rust_cli_base::names::env::USERNAME)
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "unknown".to_owned());
    }
    user_database_name().unwrap_or_else(|| "unknown".to_owned())
}

/// `~` 与 `~/…` 展开为 `home` 下的路径；其余（含 `~user`、Windows 上的 `~\…`）原样返回（Node `resolveUserPath`）。
///
/// `~` 之后多余的 `/` 会被去掉，`~//x` 得到 `home/x`，而不是根目录下的 `/x`（Node `path.join` 相同）。
///
/// F57 缺陷修复：以前 Windows 上也认 `~\…`，Node 只认 `~/`，同一个配置值两边落在不同的位置。
pub fn expand_home(input: &str, home: &Path) -> PathBuf {
    if input == "~" {
        return home.to_path_buf();
    }
    match input.strip_prefix("~/") {
        Some(rest) => home.join(rest.trim_start_matches('/')),
        None => PathBuf::from(input),
    }
}

#[derive(Clone, Copy)]
#[cfg_attr(not(unix), allow(dead_code, reason = "只有 Unix 查系统用户数据库"))]
enum Field {
    Name,
    Home,
}

#[cfg(unix)]
fn user_database_home() -> io::Result<PathBuf> {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    // Node `os.homedir()` 的回退查的是实际用户（getuid）。
    user_database_field(unsafe_uid(false), Field::Home)
        .map(|bytes| PathBuf::from(OsStr::from_bytes(&bytes)))
}

#[cfg(unix)]
fn user_database_name() -> Option<String> {
    // Node `os.userInfo()` 查的是有效用户（geteuid）。
    user_database_field(unsafe_uid(true), Field::Name)
        .ok()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(not(unix))]
fn user_database_name() -> Option<String> {
    None
}

#[cfg(unix)]
#[allow(unsafe_code, reason = "getuid / geteuid 是读取用户 ID 的唯一途径")]
fn unsafe_uid(effective: bool) -> libc::uid_t {
    if effective {
        // SAFETY: geteuid 没有前置条件，不会失败。
        unsafe { libc::geteuid() }
    } else {
        // SAFETY: getuid 没有前置条件，不会失败。
        unsafe { libc::getuid() }
    }
}

#[cfg(unix)]
#[allow(
    unsafe_code,
    reason = "getpwuid_r 是读取系统用户数据库的唯一途径（与 Node os.homedir() / os.userInfo() 的回退一致）"
)]
fn user_database_field(uid: libc::uid_t, field: Field) -> io::Result<Vec<u8>> {
    use std::ffi::CStr;
    use std::mem::MaybeUninit;

    // 条目里的字符串都放在调用方提供的缓冲区中；ERANGE 时加倍重试，上限防止异常的 NSS 实现无限增长。
    const MAX_BUFFER: usize = 1 << 20;
    let mut buffer = vec![0_u8; 1024];
    loop {
        let mut entry = MaybeUninit::<libc::passwd>::uninit();
        let mut found: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: entry、buffer、found 都是本函数持有的有效可写内存，传入的长度就是 buffer 的实际长度。
        let code = unsafe {
            libc::getpwuid_r(
                uid,
                entry.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut found,
            )
        };
        if code == libc::ERANGE && buffer.len() < MAX_BUFFER {
            buffer.resize(buffer.len() * 2, 0);
            continue;
        }
        if code != 0 {
            return Err(unavailable(
                HomeRule::Unix,
                Some(io::Error::from_raw_os_error(code)),
            ));
        }
        if found.is_null() {
            return Err(unavailable(HomeRule::Unix, None));
        }
        // SAFETY: 返回 0 且 found 非空时，found 指向已初始化的 entry；pw_dir / pw_name 为空指针或指向
        // buffer 中以 NUL 结尾的字符串，buffer 在本作用域内一直有效。
        let value = unsafe {
            let value = match field {
                Field::Home => (*found).pw_dir,
                Field::Name => (*found).pw_name,
            };
            if value.is_null() {
                return Err(unavailable(HomeRule::Unix, None));
            }
            CStr::from_ptr(value).to_bytes().to_vec()
        };
        if value.is_empty() {
            return Err(unavailable(HomeRule::Unix, None));
        }
        return Ok(value);
    }
}

#[cfg(not(unix))]
fn user_database_home() -> io::Result<PathBuf> {
    Err(unavailable(HomeRule::Unix, None))
}

fn unavailable(rule: HomeRule, cause: Option<io::Error>) -> io::Error {
    let message = match rule {
        HomeRule::Unix => {
            "home directory unavailable: HOME is unset or empty, \
             and the user database has no home directory for the current user"
        }
        HomeRule::Windows => {
            "home directory unavailable: USERPROFILE is unset or empty, \
             and the system has no profile directory for the current user"
        }
    };
    match cause {
        Some(cause) => io::Error::new(cause.kind(), format!("{message}: {cause}")),
        None => io::Error::new(io::ErrorKind::NotFound, message),
    }
}

#[cfg(test)]
#[path = "home_tests.rs"]
mod tests;
