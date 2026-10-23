#![allow(clippy::disallowed_methods, reason = "测试：在临时目录里读回交接文件")]
use super::*;

fn vars(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

/// update.md §8 的表：有交接变量时交接（不论平台）；没有时 Unix 在本进程执行、Windows 只打印命令。空值按没有。
#[test]
fn the_route_follows_the_handoff_variable_then_the_platform() {
    let handoff = vars(&[(VARIABLE, "/tmp/zcode-update-1.json")]);
    for windows in [false, true] {
        assert_eq!(
            route(&handoff, windows),
            Route::Handoff(PathBuf::from("/tmp/zcode-update-1.json"))
        );
    }
    assert_eq!(route(&[], false), Route::Run);
    assert_eq!(route(&[], true), Route::Manual);
    let empty = vars(&[(VARIABLE, "")]);
    assert_eq!(route(&empty, false), Route::Run);
    assert_eq!(route(&empty, true), Route::Manual);
}

/// 一行 JSON 加换行，只有 `version`；版本原样写（校验在启动器，update.md §8.1）。
#[test]
fn the_handoff_file_holds_only_the_version() {
    assert_eq!(render("0.1.55"), "{\"version\":\"0.1.55\"}\n");
    assert_eq!(render(LATEST), "{\"version\":\"latest\"}\n");
    // 引号与反斜杠照 JSON 转义：写的是数据，启动器不会把它当命令。
    assert_eq!(render("1\" & x"), "{\"version\":\"1\\\" & x\"}\n");
}

#[tokio::test]
async fn the_handoff_is_written_atomically_into_an_existing_directory() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("zcode-update-1.json");
    write(&path, "0.1.55").await.unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "{\"version\":\"0.1.55\"}\n"
    );
    // 不留临时文件。
    let names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["zcode-update-1.json"]);
    // 目录不存在时报错，不替启动器建目录。
    let missing = dir.path().join("missing").join("handoff.json");
    assert!(write(&missing, LATEST).await.is_err());
    assert!(!dir.path().join("missing").exists());
}

#[test]
fn windows_without_a_handoff_names_the_command_to_run() {
    assert_eq!(
        manual("npm install -g @scope/zcode"),
        "Error: Cannot update ZCode while it is running on Windows. Please run `npm install -g @scope/zcode` after it exits."
    );
}
