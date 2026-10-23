//! bwrap 挂载点的清理（R2-16 的后续）：最后一个 sandbox 结束时才清，只删还是空文件的挂载点。
#![allow(clippy::disallowed_methods, reason = "测试直接准备文件系统夹具")]

use super::*;

#[test]
fn cleanup_waits_for_the_last_sandbox() {
    let mut mounts = Mounts::default();
    mounts.enter(vec![PathBuf::from("/w/.vscode")]);
    mounts.enter(vec![PathBuf::from("/w/.zcode")]);
    assert!(mounts.leave().is_empty());
    let mut created = mounts.leave();
    created.sort();
    assert_eq!(
        created,
        [PathBuf::from("/w/.vscode"), PathBuf::from("/w/.zcode")]
    );
    assert!(mounts.leave().is_empty());
}

#[tokio::test]
async fn only_empty_files_are_removed() {
    let dir = tempfile::tempdir().unwrap();
    let empty = dir.path().join(".vscode");
    let written = dir.path().join(".bashrc");
    let folder = dir.path().join(".zcode");
    std::fs::write(&empty, "").unwrap();
    std::fs::write(&written, "x").unwrap();
    std::fs::create_dir(&folder).unwrap();
    let gone = dir.path().join("gone");
    clean(vec![empty.clone(), written.clone(), folder.clone(), gone]).await;
    assert!(!empty.exists());
    assert!(written.exists());
    assert!(folder.exists());
}
