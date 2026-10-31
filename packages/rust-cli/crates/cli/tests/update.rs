//! `zcode update` 的真实进程（docs/specs/rust-cli/update.md §8、§9，工作线 F48）：临时 HOME、临时 PATH 里放假的包管理器
//! （把参数写进文件），不运行真的 npm、不联网。工作线 F66 起三平台都编译：假的包管理器是 sh 脚本，起包管理器的两项只在
//! Unix 上跑（Windows 上不在本进程执行，见 `windows_process.rs`）；其余不起包管理器的三平台相同。
#![allow(
    clippy::disallowed_methods,
    reason = "测试在临时目录里放假的包管理器、读回它记下的参数"
)]
#![allow(
    clippy::unwrap_used,
    reason = "集成测试：辅助函数不在 #[test] 里，clippy 的测试豁免覆盖不到"
)]

mod system_env;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::{Output, Stdio};
use system_env::KeepSystem as _;

/// 假的包管理器：每次被调用时在 `$FAKE_LOG` 里追加一行 `<程序名> <参数…>`，以 `$FAKE_EXIT` 退出。
fn fake_managers(bin: &Path) {
    std::fs::create_dir_all(bin).unwrap();
    for name in ["npm", "pnpm", "bun"] {
        let path = bin.join(name);
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\necho \"{name} $*\" >> \"$FAKE_LOG\"\necho \"fake {name} output\"\nexit \"${{FAKE_EXIT:-0}}\"\n"
            ),
        )
        .unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn zcode(home: &Path, args: &[&str], vars: &[(&str, &str)]) -> Output {
    let bin = home.join("bin");
    let mut command = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"));
    command
        .args(args)
        .current_dir(home)
        .env_clear()
        .keep_system()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env(
            "PATH",
            std::env::join_paths([bin.as_path(), Path::new("/usr/bin"), Path::new("/bin")])
                .unwrap(),
        )
        .env("FAKE_LOG", home.join("calls.log"))
        .env("ZCODE_DATA_BASE_DIR", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in vars {
        command.env(key, value);
    }
    command.output().unwrap()
}

fn calls(home: &Path) -> String {
    std::fs::read_to_string(home.join("calls.log")).unwrap_or_default()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

const NPM: [(&str, &str); 3] = [
    ("ZCODE_MANAGED_BY_NPM", "1"),
    ("ZCODE_MANAGED_PACKAGE", "@scope/zcode"),
    (
        "ZCODE_MANAGED_PLATFORM_PACKAGE",
        "@scope/zcode-darwin-arm64",
    ),
];

/// 没有启动器标记（本地构建、cargo 构建、直接运行可执行文件）：报错让用户手动更新、给出发布页（工作线 F71），不起包管理器。
#[test]
fn without_a_marker_the_update_command_asks_for_a_manual_update() {
    let home = tempfile::tempdir().unwrap();
    fake_managers(&home.path().join("bin"));
    for args in [["update"], ["upgrade"]] {
        let output = zcode(home.path(), &args, &[]);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert_eq!(
            text(&output.stderr),
            "Error: Could not detect the ZCode installation method. Please update manually: https://www.npmjs.com/package/@mbearo/zcode\n"
        );
        assert!(output.stdout.is_empty(), "{output:?}");
    }
    assert_eq!(calls(home.path()), "");
    // 不建存储：HOME 下只有放假包管理器的 bin。
    let names: Vec<String> = std::fs::read_dir(home.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["bin"]);
}

/// 有标记：不检查版本，直接按安装方式执行，stdio 继承，打印 update.md §8 的文字。
#[cfg(unix)]
#[test]
fn the_update_command_runs_the_package_manager_of_the_launcher() {
    for (marker, expected) in [
        ("ZCODE_MANAGED_BY_NPM", "npm install -g @scope/zcode"),
        ("ZCODE_MANAGED_BY_PNPM", "pnpm add -g @scope/zcode"),
        ("ZCODE_MANAGED_BY_BUN", "bun install -g @scope/zcode"),
    ] {
        let home = tempfile::tempdir().unwrap();
        fake_managers(&home.path().join("bin"));
        let output = zcode(
            home.path(),
            &["update"],
            &[(marker, "1"), ("ZCODE_MANAGED_PACKAGE", "@scope/zcode")],
        );
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        assert_eq!(calls(home.path()), format!("{expected}\n"));
        let program = expected.split(' ').next().unwrap();
        assert_eq!(
            text(&output.stdout),
            format!(
                "\nUpdating ZCode via `{expected}`...\nfake {program} output\n\n🎉 Update ran successfully! Please restart ZCode.\n"
            )
        );
        assert!(output.stderr.is_empty(), "{output:?}");
    }
}

#[cfg(unix)]
#[test]
fn a_failed_update_reports_the_status_and_exits_with_one() {
    let home = tempfile::tempdir().unwrap();
    fake_managers(&home.path().join("bin"));
    let mut vars = NPM.to_vec();
    vars.push(("FAKE_EXIT", "7"));
    let output = zcode(home.path(), &["update"], &vars);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(
        text(&output.stderr),
        "Error: `npm install -g @scope/zcode` failed with status exit status: 7\n"
    );
    assert_eq!(
        text(&output.stdout),
        "\nUpdating ZCode via `npm install -g @scope/zcode`...\nfake npm output\n"
    );
}

#[test]
fn the_update_command_has_commander_help_and_errors() {
    let home = tempfile::tempdir().unwrap();
    fake_managers(&home.path().join("bin"));
    let help = zcode(home.path(), &["update", "--help"], &NPM);
    assert_eq!(help.status.code(), Some(0), "{help:?}");
    assert_eq!(
        text(&help.stdout),
        "Usage: zcode update|upgrade [options]\n\nUpdate ZCode to the latest version\n\nOptions:\n  -h, --help  Display help for command\n"
    );
    for (args, expected) in [
        (vec!["update", "--yes"], "error: unknown option '--yes'\n"),
        (
            vec!["upgrade", "latest"],
            "error: too many arguments for 'update'. Expected 0 arguments but got 1.\n",
        ),
    ] {
        let output = zcode(home.path(), &args, &NPM);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert_eq!(text(&output.stderr), expected);
    }
    assert_eq!(calls(home.path()), "");
    // `zcode --help` 的命令一节列出它（cli-args.md §3.2）。
    let page = zcode(home.path(), &["--help"], &[]);
    assert!(
        text(&page.stdout).contains(
            "  update|upgrade                        Update ZCode to the latest version\n"
        ),
        "{page:?}"
    );
}

/// Windows 的交接（update.md §8.1，工作线 F66）：有交接变量时 `zcode update` 只写交接文件（目标是 `latest`），不打印、
/// 以 0 退出、不起包管理器；启动器在它退出后执行。二进制只看变量，不看平台，所以在 macOS 上也能测。
#[test]
fn with_a_handoff_the_update_command_writes_latest_and_runs_nothing() {
    let home = tempfile::tempdir().unwrap();
    fake_managers(&home.path().join("bin"));
    let handoff = home.path().join("zcode-update-handoff.json");
    let handoff_text = handoff.display().to_string();
    let mut vars = NPM.to_vec();
    vars.push(("ZCODE_MANAGED_UPDATE_HANDOFF", &handoff_text));
    let output = zcode(home.path(), &["update"], &vars);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert_eq!(calls(home.path()), "");
    assert_eq!(
        std::fs::read_to_string(&handoff).unwrap(),
        "{\"version\":\"latest\"}\n"
    );
    // 没有标记（未知安装）时交接变量不起作用：照旧报错，不写文件。
    std::fs::remove_file(&handoff).unwrap();
    let output = zcode(
        home.path(),
        &["update"],
        &[("ZCODE_MANAGED_UPDATE_HANDOFF", &handoff_text)],
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(
        text(&output.stderr),
        "Error: Could not detect the ZCode installation method. Please update manually: https://www.npmjs.com/package/@mbearo/zcode\n"
    );
    assert!(!handoff.exists());
    // 交接文件的目录不存在：报系统错误、退出码 1，不起包管理器。
    let missing = home.path().join("missing").join("handoff.json");
    let missing_text = missing.display().to_string();
    let mut vars = NPM.to_vec();
    vars.push(("ZCODE_MANAGED_UPDATE_HANDOFF", &missing_text));
    let output = zcode(home.path(), &["update"], &vars);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(text(&output.stderr).starts_with("Error: "), "{output:?}");
    assert_eq!(calls(home.path()), "");
}
