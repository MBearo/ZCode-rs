use super::*;

#[tokio::test]
async fn run_reports_the_exit_and_a_program_that_cannot_start() {
    let args = |script: &str| [OsString::from("-c"), OsString::from(script)];
    assert_eq!(System.run("/bin/sh", &args("exit 0")).await, Ok(true));
    assert_eq!(System.run("/bin/sh", &args("exit 3")).await, Ok(false));
    assert!(System.run("/nonexistent/open", &[]).await.is_err());
}

#[tokio::test]
async fn start_reports_why_a_program_cannot_start() {
    let dir = tempfile::tempdir().unwrap();
    // 存在但不可执行（是个目录）：spawn 时就失败。
    assert!(System.start(dir.path(), &[]).await.is_err());
    assert!(
        System
            .start(&dir.path().join("missing"), &[])
            .await
            .is_err()
    );
}

#[test]
fn the_desktop_does_not_inherit_electron_run_as_node() {
    let command = launch_command(
        Path::new("/opt/ZCode/zcode"),
        &[OsString::from("--open-workspace")],
    );
    let removed = command
        .as_std()
        .get_envs()
        .any(|(key, value)| key == "ELECTRON_RUN_AS_NODE" && value.is_none());
    assert!(removed);
}

#[tokio::test]
async fn there_is_no_registry_off_windows() {
    assert_eq!(System.install_location(Hive::CurrentUser, "x").await, None);
}
