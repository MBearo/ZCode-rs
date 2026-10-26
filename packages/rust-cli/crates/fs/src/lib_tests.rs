// 复制自 rust/crates/kit/src/fs_tests.rs（feat/rs 79b6691c55）。
use std::path::Path;
use std::time::Duration;

use super::*;

const NO_RETRY: &[Duration] = &[];

fn options(mode: FileMode, temp_name: TempName, parent: ParentDir) -> WriteOptions {
    WriteOptions {
        mode,
        temp_name,
        parent,
        rename_retry: NO_RETRY,
    }
}

fn entries(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn replaces_the_target_and_leaves_no_temp_file() {
    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("setting.json");
    std::fs::write(&target, "old").unwrap();
    for style in [TempName::Hidden, TempName::Visible] {
        let content = format!("{{\"style\":\"{style:?}\"}}");
        atomic_write(
            &target,
            content.as_bytes(),
            options(FileMode::Default, style, ParentDir::Existing),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), content);
        assert_eq!(entries(directory.path()), vec!["setting.json".to_owned()]);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn private_files_are_exactly_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("credentials.json");
    std::fs::write(&target, "{}").unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
    atomic_write(
        &target,
        b"{\"k\":1}",
        options(FileMode::Private, TempName::Hidden, ParentDir::Existing),
    )
    .await
    .unwrap();
    let mode = std::fs::metadata(&target).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    assert_eq!(std::fs::read(&target).unwrap(), b"{\"k\":1}");
}

/// 下载的执行器（remote-exec.md §26.4）：不论 umask，恰好 0755，内容完整。
#[cfg(unix)]
#[tokio::test]
async fn executable_files_are_exactly_0755() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("zcode-rs");
    atomic_write(
        &target,
        b"\x7fELF",
        options(FileMode::Executable, TempName::Hidden, ParentDir::Existing),
    )
    .await
    .unwrap();
    let mode = std::fs::metadata(&target).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o755);
    assert_eq!(std::fs::read(&target).unwrap(), b"\x7fELF");
    assert_eq!(entries(directory.path()), ["zcode-rs"]);
}

#[tokio::test]
async fn creates_missing_parents_only_when_asked() {
    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("a/b/config.json");
    let missing = atomic_write(
        &target,
        b"x",
        options(FileMode::Default, TempName::Hidden, ParentDir::Existing),
    )
    .await;
    assert_eq!(missing.unwrap_err().kind(), ErrorKind::NotFound);
    assert!(entries(directory.path()).is_empty());
    atomic_write(
        &target,
        b"x",
        options(FileMode::Default, TempName::Hidden, ParentDir::Create),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"x");
}

#[tokio::test]
async fn failed_rename_removes_the_temp_file_and_keeps_the_target() {
    let directory = tempfile::tempdir().unwrap();
    // 目标是一个非空目录：rename 失败且不可重试。
    let target = directory.path().join("occupied");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("keep"), "kept").unwrap();
    for style in [TempName::Hidden, TempName::Visible] {
        let result = atomic_write(
            &target,
            b"new",
            options(FileMode::Private, style, ParentDir::Existing),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(entries(directory.path()), vec!["occupied".to_owned()]);
        assert_eq!(
            std::fs::read_to_string(target.join("keep")).unwrap(),
            "kept"
        );
    }
}

#[test]
fn temp_names_follow_the_node_conventions() {
    let pid = std::process::id().to_string();
    let hidden = temp_name(OsStr::new("credentials.json"), TempName::Hidden);
    let visible = temp_name(OsStr::new("setting.json"), TempName::Visible);
    let (hidden, visible) = (hidden.to_str().unwrap(), visible.to_str().unwrap());
    for (name, prefix) in [(hidden, ".credentials.json."), (visible, "setting.json.")] {
        let rest = name.strip_prefix(prefix).unwrap();
        let parts: Vec<&str> = rest.strip_suffix(".tmp").unwrap().split('.').collect();
        assert_eq!(parts.len(), 3, "{name}");
        assert_eq!(parts[0], pid);
        assert!(parts[1].parse::<u128>().is_ok(), "{name}");
        assert_eq!(parts[2].len(), 16, "{name}");
    }
    assert_ne!(
        temp_name(OsStr::new("a"), TempName::Hidden),
        temp_name(OsStr::new("a"), TempName::Hidden)
    );
}

#[test]
fn only_busy_or_denied_renames_are_retried() {
    for (kind, expected) in [
        (ErrorKind::PermissionDenied, true),
        (ErrorKind::ResourceBusy, true),
        (ErrorKind::NotFound, false),
        (ErrorKind::IsADirectory, false),
    ] {
        assert_eq!(retryable(&io::Error::from(kind)), expected, "{kind:?}");
    }
}

#[tokio::test]
async fn rejects_targets_without_a_file_name() {
    let result = atomic_write(
        Path::new("/"),
        b"x",
        options(FileMode::Default, TempName::Hidden, ParentDir::Existing),
    )
    .await;
    assert_eq!(result.unwrap_err().kind(), ErrorKind::InvalidInput);
}

#[tokio::test]
async fn read_optional_distinguishes_missing_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.json");
    assert_eq!(read_optional(&path).await.unwrap(), None);
    std::fs::write(&path, b"{}").unwrap();
    assert_eq!(read_optional(&path).await.unwrap(), Some(b"{}".to_vec()));
    // 读目录是错误，不当作“不存在”。
    assert!(read_optional(dir.path()).await.is_err());
}

#[tokio::test]
async fn appends_create_the_parents_and_keep_the_order() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tui/history.jsonl");
    append(&path, b"one\n").await.unwrap();
    append(&path, b"two\n").await.unwrap();
    let text = read_optional(&path).await.unwrap().unwrap();
    assert_eq!(text, b"one\ntwo\n");
}

#[tokio::test]
async fn subdirectories_lists_child_directories_by_name() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("b/inner")).unwrap();
    std::fs::create_dir_all(dir.path().join("a")).unwrap();
    std::fs::write(dir.path().join("file.json"), b"{}").unwrap();
    // 指向目录的符号链接不算（Node `readdir` 的 `isDirectory()` 不跟随链接）。
    #[cfg(unix)]
    std::os::unix::fs::symlink(dir.path().join("a"), dir.path().join("link")).unwrap();
    assert_eq!(subdirectories(dir.path()).await.unwrap(), ["a", "b"]);
    let missing = subdirectories(&dir.path().join("missing")).await;
    assert_eq!(missing.unwrap_err().kind(), ErrorKind::NotFound);
}

#[tokio::test]
async fn file_len_modified_and_names_tolerate_missing_paths() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("b.jsonl"), b"12345").unwrap();
    std::fs::write(dir.path().join("a.jsonl"), b"").unwrap();
    assert_eq!(
        file_len(&dir.path().join("b.jsonl")).await.unwrap(),
        Some(5)
    );
    assert_eq!(file_len(&dir.path().join("missing")).await.unwrap(), None);
    // 修改时间（F91）：文件不存在时为空。
    let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    std::fs::File::options()
        .write(true)
        .open(dir.path().join("b.jsonl"))
        .unwrap()
        .set_modified(when)
        .unwrap();
    assert_eq!(
        file_modified(&dir.path().join("b.jsonl")).await.unwrap(),
        Some(when)
    );
    assert_eq!(
        file_modified(&dir.path().join("missing")).await.unwrap(),
        None
    );
    // 只列普通文件，按名字排序；目录不存在时为空。
    assert_eq!(
        file_names(dir.path()).await.unwrap(),
        ["a.jsonl", "b.jsonl"]
    );
    assert!(
        file_names(&dir.path().join("missing"))
            .await
            .unwrap()
            .is_empty()
    );
}

#[test]
fn the_node_rename_retry_delays_keep_their_values() {
    // F55：凭据与信任记录共用这一份；值同 Node `DEFAULT_RENAME_RETRY_DELAYS_MS`。
    let millis: Vec<u128> = NODE_RENAME_RETRY.iter().map(Duration::as_millis).collect();
    assert_eq!(millis, [50, 100, 200, 400, 800]);
}
