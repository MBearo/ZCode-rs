//! 可执行文件名与命令名（docs/specs/rust-cli/cli-args.md §13，工作线 F59）：cargo 产出的可执行文件是 `zcode-rs`，
//! 用户敲的命令是 `zcode`（npm 包的 bin 名）。把产物复制成这两个名字（Windows 带 `.exe`；`zcode.exe` 也是 Windows
//! 平台包里的名字，工作线 F71）都走正常入口：argv0 分派
//! 只认 rg / bfs / ugrep；帮助与版本里的程序名固定是 `zcode`，不看 argv[0]。临时 HOME，不读真实配置。
#![allow(
    clippy::disallowed_methods,
    reason = "测试复制二进制、检查临时 HOME 里有没有写出文件"
)]
#![allow(
    clippy::unwrap_used,
    reason = "集成测试：辅助函数不在 #[test] 里，clippy 的测试豁免覆盖不到"
)]

mod system_env;
use std::env::consts::EXE_SUFFIX;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use system_env::KeepSystem as _;

const BUILT: &str = env!("CARGO_BIN_EXE_zcode-rs");

/// 把产物复制成 `dir/<name>`（Windows 加 `.exe`）。
fn copy_as(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(format!("{name}{EXE_SUFFIX}"));
    std::fs::copy(BUILT, &path).unwrap();
    path
}

fn run(program: &Path, flag: &str, home: &Path) -> Output {
    rust_cli_proc::std_command(program)
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
fn the_built_executable_is_zcode_rs() {
    assert_eq!(
        Path::new(BUILT).file_name().unwrap().to_str(),
        Some(format!("zcode-rs{EXE_SUFFIX}").as_str())
    );
}

#[test]
fn copies_named_zcode_rs_or_zcode_start_normally() {
    let version = env!("ZCODE_BUILD_VERSION");
    // 放在产物旁边：同一个卷上复制更快（APFS 上是克隆）。
    let dir = tempfile::tempdir_in(Path::new(BUILT).parent().unwrap()).unwrap();
    for name in ["zcode-rs", "zcode"] {
        let program = copy_as(dir.path(), name);
        let home = tempfile::tempdir().unwrap();

        let output = run(&program, "--version", home.path());
        assert!(output.status.success(), "{name}: {output:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            format!("zcode {version} (rust-cli)\n"),
            "{name}"
        );

        // 帮助里的程序名是命令名 `zcode`，与可执行文件叫什么无关。
        let help = run(&program, "--help", home.path());
        assert!(help.status.success(), "{name}: {help:?}");
        assert_eq!(
            String::from_utf8_lossy(&help.stdout).lines().next(),
            Some("Usage: zcode [options] [command] [prompt]"),
            "{name}"
        );

        // 不读配置、不建存储：临时 HOME 仍是空的。
        assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0, "{name}");
    }
}
