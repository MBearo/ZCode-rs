//! `zcode --version` / `-v` / `-V` 的真实进程（docs/specs/rust-cli/version.md §8）：临时 HOME，不读任何真实
//! 配置；输出 `zcode <版本> (rust-cli)`，版本来自 build.rs 给的 `ZCODE_BUILD_VERSION`（与二进制同一次编译）。
#![allow(
    clippy::disallowed_methods,
    reason = "测试检查临时 HOME 里有没有写出文件，release 下读仓库里的计数文件"
)]
#![allow(
    clippy::unwrap_used,
    reason = "集成测试：辅助函数不在 #[test] 里，clippy 的测试豁免覆盖不到"
)]

mod system_env;
use std::path::Path;
use std::process::{Output, Stdio};
use system_env::KeepSystem as _;

fn zcode(home: &Path, flag: &str) -> Output {
    rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"))
        .arg(flag)
        .env_clear()
        .keep_system()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("ZCODE_DATA_BASE_DIR", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap()
}

#[test]
fn every_version_flag_prints_the_build_version() {
    let version = env!("ZCODE_BUILD_VERSION");
    let home = tempfile::tempdir().unwrap();
    for flag in ["--version", "-v", "-V"] {
        let output = zcode(home.path(), flag);
        assert!(output.status.success(), "{flag}: {output:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            format!("zcode {version} (rust-cli)\n"),
            "{flag}"
        );
        assert!(output.stderr.is_empty(), "{flag}: {output:?}");
    }
    // 不读配置、不建存储：临时 HOME 仍是空的。
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
}

#[test]
fn the_build_version_follows_the_profile() {
    let version = env!("ZCODE_BUILD_VERSION");
    let parts: Vec<&str> = version.split('.').collect();
    assert_eq!(parts.len(), 3, "{version}");
    // major、minor 总是来自 Cargo.toml。
    assert_eq!(parts[0], env!("CARGO_PKG_VERSION_MAJOR"));
    assert_eq!(parts[1], env!("CARGO_PKG_VERSION_MINOR"));
    if cfg!(debug_assertions) {
        // debug 编译：Cargo.toml 的版本原样不变，不看计数文件，也不看发版变量（version.md §1）。
        assert_eq!(version, env!("CARGO_PKG_VERSION"));
        return;
    }
    // release 编译：第三位来自仓库里的计数文件（version.md §2）。
    let count: u64 = parts[2].parse().unwrap();
    let counter = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../build-number");
    let committed: u64 = std::fs::read_to_string(counter)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    if let Some(published) = option_env!("ZCODE_RELEASE_VERSION") {
        // 正式发版：等于变量，文件只读、没有加 1。
        assert_eq!(version, published);
        assert_eq!(count, committed, "{version}");
    } else {
        // 本地 release：编译时加过 1；之后别的 release 编译可能又加过。
        assert!((1..=committed).contains(&count), "{version} / {committed}");
    }
}
