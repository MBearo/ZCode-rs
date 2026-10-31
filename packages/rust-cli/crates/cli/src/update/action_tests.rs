#![allow(
    clippy::disallowed_methods,
    reason = "测试：在临时目录里放假的包管理器，读回它记下的参数与交接文件"
)]
#[cfg(unix)]
use std::path::Path;

use super::super::install::Manager;
use super::*;

fn install(manager: Manager) -> Install {
    Install {
        manager,
        package: "@scope/zcode".to_owned(),
        platform_package: None,
    }
}

/// 可执行的假文件。
#[cfg(unix)]
fn executable(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// 假的包管理器：把参数逐行写进 `$FAKE_ARGS`，以 `$FAKE_EXIT` 退出。
#[cfg(unix)]
fn fake_manager(dir: &Path, name: &str) {
    executable(
        &dir.join(name),
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$FAKE_ARGS\"\nexit \"${FAKE_EXIT:-0}\"\n",
    );
}

#[cfg(unix)]
fn env(pairs: &[(&str, String)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect()
}

#[cfg(unix)]
#[tokio::test]
async fn the_package_manager_gets_the_install_arguments_and_the_result_is_printed() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for (manager, name, expected) in [
        (Manager::Npm, "npm", "install\n-g\n@scope/zcode\n"),
        (Manager::Pnpm, "pnpm", "add\n-g\n@scope/zcode\n"),
        (Manager::Bun, "bun", "install\n-g\n@scope/zcode\n"),
    ] {
        fake_manager(&bin, name);
        let args = dir.path().join(format!("{name}.args"));
        let vars = env(&[
            ("PATH", format!("{}:/usr/bin:/bin", bin.display())),
            ("FAKE_ARGS", args.display().to_string()),
        ]);
        let mut out = Vec::new();
        let code = run(&install(manager), "0.1.55", &vars, &mut out)
            .await
            .unwrap();
        assert_eq!(code, 0);
        // Unix 照旧不带版本（版本只给交接文件）。
        assert_eq!(std::fs::read_to_string(&args).unwrap(), expected);
        let line = install(manager).command_line();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!(
                "\nUpdating ZCode via `{line}`...\n\n🎉 Update ran successfully! Please restart ZCode.\n"
            )
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_failing_package_manager_is_an_error_with_its_status() {
    let dir = tempfile::tempdir().unwrap();
    fake_manager(dir.path(), "npm");
    let vars = env(&[
        ("PATH", format!("{}:/usr/bin:/bin", dir.path().display())),
        ("FAKE_ARGS", dir.path().join("args").display().to_string()),
        ("FAKE_EXIT", "3".to_owned()),
    ]);
    let mut out = Vec::new();
    let error = run(&install(Manager::Npm), "0.1.55", &vars, &mut out)
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Error: `npm install -g @scope/zcode` failed with status exit status: 3"
    );
    assert!(error.is::<Fatal>());
    // 开头的那句已经打印，没有成功的那句。
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "\nUpdating ZCode via `npm install -g @scope/zcode`...\n"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_missing_package_manager_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let vars = env(&[("PATH", dir.path().display().to_string())]);
    let error = run(&install(Manager::Bun), "0.1.55", &vars, &mut Vec::new())
        .await
        .unwrap_err();
    assert!(error.is::<Fatal>());
    assert!(
        error
            .to_string()
            .starts_with("Error: No such file or directory"),
        "{error}"
    );
}

/// 交接（update.md §8.1，F66）：只写目标版本，不起包管理器、不打印，以 0 退出。PATH 里放了假的包管理器，它不该被调用。
#[cfg(unix)]
#[tokio::test]
async fn a_handoff_writes_the_version_and_runs_nothing() {
    let dir = tempfile::tempdir().unwrap();
    fake_manager(dir.path(), "npm");
    let args = dir.path().join("args");
    let handoff = dir.path().join("zcode-update-1.json");
    let vars = env(&[
        ("PATH", format!("{}:/usr/bin:/bin", dir.path().display())),
        ("FAKE_ARGS", args.display().to_string()),
        (handoff::VARIABLE, handoff.display().to_string()),
    ]);
    let mut out = Vec::new();
    let code = run(&install(Manager::Npm), "0.1.55", &vars, &mut out)
        .await
        .unwrap();
    assert_eq!(code, 0);
    assert!(out.is_empty());
    assert!(!args.exists());
    assert_eq!(
        std::fs::read_to_string(&handoff).unwrap(),
        "{\"version\":\"0.1.55\"}\n"
    );
}

/// 交接文件写不进去（目录不存在）：系统错误，退出码 1（Fatal），不打印。
#[tokio::test]
async fn a_handoff_that_cannot_be_written_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing").join("handoff.json");
    let mut out = Vec::new();
    let error = execute(
        Route::Handoff(path),
        &install(Manager::Npm),
        "latest",
        &[],
        &mut out,
    )
    .await
    .unwrap_err();
    assert!(error.is::<Fatal>());
    assert!(error.to_string().starts_with("Error: "), "{error}");
    assert!(out.is_empty());
}

/// Windows 上有标记、没有交接变量（不是经 Windows 的启动器启动）：不执行，报出要用户自己执行的命令。三平台都按 Windows 的
/// 做法验证。
#[tokio::test]
async fn windows_without_a_handoff_only_names_the_command() {
    for (manager, line) in [
        (Manager::Npm, "npm install -g @scope/zcode"),
        (Manager::Pnpm, "pnpm add -g @scope/zcode"),
        (Manager::Bun, "bun install -g @scope/zcode"),
    ] {
        let mut out = Vec::new();
        let error = execute(Route::Manual, &install(manager), "0.1.55", &[], &mut out)
            .await
            .unwrap_err();
        assert!(error.is::<Fatal>());
        assert_eq!(
            error.to_string(),
            format!(
                "Error: Cannot update ZCode while it is running on Windows. Please run `{line}` after it exits."
            )
        );
        assert!(out.is_empty());
    }
}
