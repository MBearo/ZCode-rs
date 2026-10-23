//! 内嵌的 rg 按 argv0 分派的真实进程（docs/specs/rust-cli/embedded-search.md §3、§5，工作线 F53）：把 zcode 链接
//! （Windows 复制）成 `rg` 运行，或用环境变量 `ARGV0=rg`（msys 的 bash 只把它放进环境）；临时 HOME，不读真实配置。
#![allow(
    clippy::disallowed_methods,
    reason = "测试在临时目录里建文件、链接二进制并读回结果"
)]
#![allow(
    clippy::unwrap_used,
    reason = "集成测试：辅助函数不在 #[test] 里，clippy 的测试豁免覆盖不到"
)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const ZCODE: &str = env!("CARGO_BIN_EXE_zcode-rs");

/// 把 zcode 放到 `dir/<name>`：Unix 上是符号链接（同 `exec -a` 改 argv[0] 的效果），Windows 上复制成 `<name>.exe`。
fn alias(dir: &Path, name: &str) -> PathBuf {
    if cfg!(windows) {
        let path = dir.join(format!("{name}.exe"));
        std::fs::copy(ZCODE, &path).unwrap();
        path
    } else {
        let path = dir.join(name);
        #[cfg(unix)]
        std::os::unix::fs::symlink(ZCODE, &path).unwrap();
        path
    }
}

fn run(program: &Path, args: &[&str], cwd: &Path, home: &Path, extra: &[(&str, &str)]) -> Output {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("ZCODE_DATA_BASE_DIR", home)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in extra {
        command.env(key, value);
    }
    command.output().unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n")
}

/// 固定的小目录：两个文件里有 `needle`，`.git` 与 `.gitignore` 里的文件被 rg 跳过。
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::write(root.join("src/a.rs"), "fn a() {}\nlet needle = 1;\n").unwrap();
    std::fs::write(root.join("b.txt"), "needle here\nnothing\n").unwrap();
    std::fs::write(root.join("ignored.log"), "needle ignored\n").unwrap();
    std::fs::write(root.join(".gitignore"), "*.log\n").unwrap();
    std::fs::write(root.join(".git/config"), "needle in git\n").unwrap();
    dir
}

fn sorted_lines(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = text.lines().map(|line| line.replace('\\', "/")).collect();
    lines.sort();
    lines
}

#[test]
fn zcode_named_rg_is_ripgrep() {
    let home = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let rg = alias(bin.path(), "rg");
    let version = run(&rg, &["--version"], home.path(), home.path(), &[]);
    assert!(version.status.success(), "{version:?}");
    let text = stdout(&version);
    assert!(text.starts_with("ripgrep 15.2.0"), "{text}");
    // rg 带 PCRE2（pcre2-sys 0.2.10 编的是 PCRE2 10.46；rg 显示的是 pcre2-sys 的 Rust 常量，写的是 10.45）。
    assert!(text.contains("features:+pcre2"), "{text}");
    assert!(text.contains("is available"), "{text}");
    // 不读配置、不建存储：临时 HOME 仍是空的。
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);

    let tree = fixture();
    let found = run(&rg, &["-n", "needle"], tree.path(), home.path(), &[]);
    assert!(found.status.success(), "{found:?}");
    assert_eq!(
        sorted_lines(&stdout(&found)),
        vec!["b.txt:1:needle here", "src/a.rs:2:let needle = 1;"]
    );
    // 没有匹配时退出码 1，与 rg 相同。
    let none = run(&rg, &["absent-word"], tree.path(), home.path(), &[]);
    assert_eq!(none.status.code(), Some(1), "{none:?}");
}

#[test]
fn the_argv0_variable_dispatches_too() {
    let home = tempfile::tempdir().unwrap();
    let tree = fixture();
    let version = run(
        Path::new(ZCODE),
        &["--version"],
        home.path(),
        home.path(),
        &[("ARGV0", "rg")],
    );
    assert!(version.status.success(), "{version:?}");
    assert!(
        stdout(&version).starts_with("ripgrep 15.2.0"),
        "{version:?}"
    );
    let found = run(
        Path::new(ZCODE),
        &["--files-with-matches", "needle"],
        tree.path(),
        home.path(),
        &[("ARGV0", "rg")],
    );
    assert_eq!(
        sorted_lines(&stdout(&found)),
        vec!["b.txt", "src/a.rs"],
        "{found:?}"
    );
}

/// 运行工具前从环境删掉 ARGV0：工具再起的子进程（rg 的 `--pre`、bfs 的 `-exec`）不继承它。
#[cfg(unix)]
#[test]
fn the_tool_does_not_pass_argv0_on() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let tree = tempfile::tempdir().unwrap();
    let script = tree.path().join("pre.sh");
    std::fs::write(&script, "#!/bin/sh\necho \"ARGV0=${ARGV0:-unset}\"\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(tree.path().join("x.txt"), "x\n").unwrap();
    let output = run(
        Path::new(ZCODE),
        &[
            "--pre",
            script.to_str().unwrap(),
            "--no-filename",
            "ARGV0",
            "x.txt",
        ],
        tree.path(),
        home.path(),
        &[("ARGV0", "rg")],
    );
    assert_eq!(stdout(&output), "ARGV0=unset\n", "{output:?}");
}

#[test]
fn other_names_are_zcode() {
    let home = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let version = env!("ZCODE_BUILD_VERSION");
    for name in ["rgx", "zcode-rg", "grep"] {
        let program = alias(bin.path(), name);
        let output = run(&program, &["--version"], home.path(), home.path(), &[]);
        assert_eq!(
            stdout(&output),
            format!("zcode {version} (rust-cli)\n"),
            "{name}: {output:?}"
        );
    }
    let output = run(
        Path::new(ZCODE),
        &["--version"],
        home.path(),
        home.path(),
        &[("ARGV0", "zcode")],
    );
    assert_eq!(stdout(&output), format!("zcode {version} (rust-cli)\n"));
}
