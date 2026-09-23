//! Node `isGitRuntimeContextUnsafe` (`bash-git-runtime-safety.ts`): whether a
//! read-only git command could load hooks or config from an untrusted git
//! directory around the working directory. Filesystem checks only, no git.
use std::{
    fs,
    path::{Component, Path, PathBuf},
};
use unicode_normalization::UnicodeNormalization;

#[derive(PartialEq)]
enum State {
    None,
    Trusted,
    Unsafe,
}

const MAX_GITDIR_FILE_BYTES: u64 = 32 * 1024;

/// Runs the blocking checks off the async workers.
pub(super) async fn unsafe_context(cwd: &Path) -> bool {
    let cwd = cwd.to_owned();
    tokio::task::spawn_blocking(move || context_unsafe(&cwd))
        .await
        .unwrap_or(true)
}

fn context_unsafe(cwd: &Path) -> bool {
    if cwd.as_os_str().is_empty() {
        return false;
    }
    let Some(canonical) = canonical(cwd) else {
        return true;
    };
    match classify_dot_git(cwd, &canonical) {
        State::Trusted => return false,
        State::Unsafe => return true,
        State::None => {}
    }
    let mut current = cwd.to_path_buf();
    loop {
        // 与 Node 一致：先看当前目录是否像裸仓库，再看父目录的 .git，
        // 所以仓库子目录里含 refs/ 也判为不安全。
        if bare_indicators(&current) {
            return true;
        }
        let Some(parent) = current.parent().map(Path::to_path_buf) else {
            return false;
        };
        match classify_dot_git(&parent, &canonical) {
            State::Trusted => return false,
            State::Unsafe => return true,
            State::None => {}
        }
        current = parent;
    }
}

/// Node `path.resolve` / `path.join`: lexical, `..` collapsed before realpath.
fn lexical(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let mut out = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Node `normalizeCanonicalPath(realpathSync.native(resolve(path)))`.
fn canonical(path: &Path) -> Option<String> {
    let real = fs::canonicalize(lexical(path)).ok()?;
    let text = real.to_string_lossy();
    // Windows 的 canonicalize 带 `\\?\` 前缀，realpathSync.native 没有。
    let text = text
        .strip_prefix(r"\\?\UNC\")
        .map(|rest| format!(r"\\{rest}"))
        .or_else(|| text.strip_prefix(r"\\?\").map(str::to_owned))
        .unwrap_or_else(|| text.into_owned());
    Some(
        text.replace('\\', "/")
            .nfc()
            .collect::<String>()
            .to_lowercase(),
    )
}

fn classify_dot_git(directory: &Path, cwd_canonical: &str) -> State {
    let dot_git = directory.join(".git");
    let Ok(meta) = fs::symlink_metadata(&dot_git) else {
        return State::None;
    };
    let join = |target: &Path| {
        if target.is_absolute() {
            target.to_path_buf()
        } else {
            lexical(&directory.join(target))
        }
    };
    if meta.file_type().is_symlink() {
        return match fs::read_link(&dot_git) {
            Ok(target) => classify_target(&join(&target), cwd_canonical),
            Err(_) => State::Unsafe,
        };
    }
    if meta.is_file() {
        if meta.len() > MAX_GITDIR_FILE_BYTES {
            return State::Unsafe;
        }
        let Ok(bytes) = fs::read(&dot_git) else {
            return State::None;
        };
        let content = String::from_utf8_lossy(&bytes);
        if content.contains('\0') {
            return State::Unsafe;
        }
        let Some(target) = content.strip_prefix("gitdir: ") else {
            return State::None;
        };
        let target = target.trim_end_matches(['\r', '\n']);
        return classify_target(&join(Path::new(target)), cwd_canonical);
    }
    if meta.is_dir() && trusted_git_directory(&dot_git) {
        return State::Trusted;
    }
    State::None
}

fn classify_target(target: &Path, cwd_canonical: &str) -> State {
    let Some(canonical) = canonical(target) else {
        return State::Unsafe;
    };
    let base = if cwd_canonical.ends_with('/') {
        cwd_canonical.to_owned()
    } else {
        format!("{cwd_canonical}/")
    };
    if canonical == cwd_canonical || canonical.starts_with(&base) {
        return State::Unsafe;
    }
    if !canonical
        .split(['\\', '/'])
        .any(|segment| segment.to_lowercase() == ".git")
    {
        return State::Unsafe;
    }
    // 与 Node 一致：HEAD 按小写化后的规范路径查找（大小写敏感文件系统上可能找不到）。
    if valid_head(Path::new(&canonical)) {
        State::Trusted
    } else {
        State::None
    }
}

fn trusted_git_directory(directory: &Path) -> bool {
    if !valid_head(directory) {
        return false;
    }
    let executable_dir = |child: &str| {
        let path = directory.join(child);
        fs::metadata(&path).is_ok_and(|m| m.is_dir()) && executable(&path)
    };
    executable_dir("objects")
        && executable_dir("refs")
        && fs::metadata(directory.join("commondir")).is_err()
}

#[cfg(unix)]
fn executable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `path` is a valid NUL-terminated string for the duration of the call.
    unsafe { libc::access(path.as_ptr(), libc::X_OK) == 0 }
}

#[cfg(not(unix))]
fn executable(_path: &Path) -> bool {
    true
}

/// `HEAD` is a small regular file naming a ref or an object id.
fn valid_head(directory: &Path) -> bool {
    let head = directory.join("HEAD");
    let Ok(meta) = fs::symlink_metadata(&head) else {
        return false;
    };
    if !meta.is_file() || meta.len() > 4096 {
        return false;
    }
    let Ok(bytes) = fs::read(&head) else {
        return false;
    };
    let text = String::from_utf8_lossy(&bytes);
    // JS `slice(0, 255)` 按 UTF-16 截断；跨界的代理对会留下半个字符，使对象 id 形式不匹配。
    let mut units = 0;
    let mut straddle = false;
    let end = text
        .char_indices()
        .find(|(_, c)| {
            units += c.len_utf16();
            straddle = units > 255 && units - c.len_utf16() < 255;
            units > 255
        })
        .map_or(text.len(), |(i, _)| i);
    let head = &text[..end];
    if let Some(rest) = head.strip_prefix("ref:") {
        return rest.trim_start_matches([' ', '\t']).starts_with("refs/");
    }
    let hex = head
        .bytes()
        .take_while(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        .count();
    !straddle
        && (hex == 40 || hex == 64)
        && head[hex..]
            .bytes()
            .all(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
}

fn bare_indicators(directory: &Path) -> bool {
    let head = fs::symlink_metadata(directory.join("HEAD"))
        .is_ok_and(|m| m.is_file() || m.file_type().is_symlink());
    head || ["objects", "refs"]
        .iter()
        .any(|child| fs::metadata(directory.join(child)).is_ok())
}

#[cfg(test)]
#[path = "git_safety_tests.rs"]
mod tests;
