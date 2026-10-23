// 复制自 rust/crates/kit/src/paths_tests.rs（feat/rs 79b6691c55）。
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::*;

fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
    move |name| {
        pairs
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| OsString::from(value))
    }
}

fn no_profile() -> Option<PathBuf> {
    None
}

/// 工作线 F57：照 Node `os.homedir()`（libuv `uv_os_homedir`）：Unix 只看 `HOME`，Windows 只看 `USERPROFILE`。
/// 以前所有平台都是 `HOME` → `USERPROFILE`，Windows 上设了 `HOME`（Git Bash 常设）时会话库与 Node 不是同一个。
#[test]
fn home_follows_node_on_each_platform() {
    let both = || env(&[("HOME", "/h"), ("USERPROFILE", r"C:\Users\dev")]);
    assert_eq!(
        home_for(HomeRule::Unix, both(), no_profile).unwrap(),
        PathBuf::from("/h")
    );
    assert_eq!(
        home_for(HomeRule::Windows, both(), no_profile).unwrap(),
        PathBuf::from(r"C:\Users\dev")
    );
    // 入口用的就是本机的规则。
    assert_eq!(
        HomeRule::CURRENT,
        if cfg!(windows) {
            HomeRule::Windows
        } else {
            HomeRule::Unix
        }
    );
}

/// 工作线 F57：Windows 上 `USERPROFILE` 没有或为空时用系统 API（入口传入 `std::env::home_dir()`，即
/// `GetUserProfileDirectoryW`），`HOME` 仍不看；系统也给不出（或给出空路径）时报错，绝不返回空路径。
#[test]
fn windows_falls_back_to_the_system_profile_not_to_home() {
    let system = || Some(PathBuf::from(r"C:\Users\system"));
    for lookup in [
        env(&[("HOME", "/h")]),
        env(&[("HOME", "/h"), ("USERPROFILE", "")]),
    ] {
        assert_eq!(
            home_for(HomeRule::Windows, lookup, system).unwrap(),
            PathBuf::from(r"C:\Users\system")
        );
    }
    let error = home_for(HomeRule::Windows, env(&[("HOME", "/h")]), no_profile).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    assert!(error.to_string().contains("USERPROFILE"), "{error}");
    let empty = || Some(PathBuf::new());
    assert!(home_for(HomeRule::Windows, env(&[]), empty).is_err());
}

/// B4：`HOME` 为空时旧实现返回空路径，数据会写到当前工作目录下。F57 起 Unix 不再看 `USERPROFILE`（Node 不看）。
#[test]
fn empty_home_never_resolves_to_an_empty_path() {
    for lookup in [
        env(&[("HOME", "")]),
        env(&[("HOME", ""), ("USERPROFILE", "/u")]),
    ] {
        let resolved = home_for(HomeRule::Unix, lookup, no_profile);
        #[cfg(unix)]
        {
            let home = resolved.unwrap();
            assert!(
                home.is_absolute(),
                "falls back to the user database, got {home:?}"
            );
            assert_ne!(home, PathBuf::from("/u"));
            assert_eq!(
                home_for(HomeRule::Unix, |_| None, no_profile).unwrap(),
                home
            );
        }
        #[cfg(not(unix))]
        assert_eq!(resolved.unwrap_err().kind(), std::io::ErrorKind::NotFound);
    }
}

#[test]
fn expands_tilde_against_home() {
    let home = Path::new("/home/u");
    let cases: &[(&str, PathBuf)] = &[
        ("~", home.to_path_buf()),
        ("~/x/y", home.join("x/y")),
        ("~//x", home.join("x")),
        ("~user/x", PathBuf::from("~user/x")),
        ("x/~", PathBuf::from("x/~")),
        ("/abs", PathBuf::from("/abs")),
        ("rel", PathBuf::from("rel")),
        ("", PathBuf::new()),
    ];
    for (input, expected) in cases {
        assert_eq!(&expand_home(input, home), expected, "input {input:?}");
    }
    // 工作线 F57：照 Node `resolveUserPath`，所有平台都只认 `~/`；`~\x` 是普通相对路径（以前 Windows 上按 home 展开）。
    assert_eq!(expand_home("~\\x", home), PathBuf::from("~\\x"));
}

#[test]
fn the_user_name_is_never_empty() {
    let name = user_name_with(env(&[("USERNAME", "dev")]));
    assert!(!name.is_empty());
    if cfg!(windows) {
        assert_eq!(name, "dev");
        assert_eq!(user_name_with(env(&[])), "unknown");
    }
}

/// 只在 Windows 上跑（CI）：本机的 home 是 `USERPROFILE`（Node `os.homedir()`），`HOME` 不看；系统 API 与它一致。
#[cfg(windows)]
#[test]
#[allow(clippy::disallowed_methods, reason = "测试对照本机的环境与系统 API")]
fn windows_home_is_the_user_profile() {
    let profile = std::env::var_os("USERPROFILE").expect("USERPROFILE on Windows");
    let home = home_dir_with(
        |name| {
            (name == "HOME")
                .then(|| OsString::from(r"C:\not-home"))
                .or_else(|| std::env::var_os(name))
        },
        std::env::home_dir,
    )
    .unwrap();
    assert_eq!(home, PathBuf::from(&profile));
    let system = home_for(HomeRule::Windows, |_| None, std::env::home_dir).unwrap();
    assert!(system.is_absolute(), "{system:?}");
}
