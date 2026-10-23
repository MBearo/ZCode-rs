//! 本用户私有的临时目录（R2-1）：建目录、改回 0700、拒绝符号链接与别人的目录。
#![allow(clippy::disallowed_methods, reason = "测试直接准备文件系统夹具")]

use super::*;

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn the_directory_name_carries_the_uid() {
    let temp = Path::new("/t");
    #[cfg(unix)]
    assert_eq!(user_dir(temp), temp.join(format!("zcode-{}", uid())));
    #[cfg(windows)]
    assert_eq!(user_dir(temp), temp.join("zcode"));
}

#[cfg(unix)]
#[tokio::test]
async fn a_missing_directory_is_created_private_and_a_loose_one_is_tightened() {
    use std::os::unix::fs::PermissionsExt;
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("a/zcode-1");
    ensure(&dir).await.unwrap();
    assert_eq!(mode(&dir), 0o700);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    ensure(&dir).await.unwrap();
    assert_eq!(mode(&dir), 0o700);
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlink_is_refused() {
    let base = tempfile::tempdir().unwrap();
    let target = base.path().join("elsewhere");
    std::fs::create_dir(&target).unwrap();
    let dir = base.path().join("zcode-1");
    std::os::unix::fs::symlink(&target, &dir).unwrap();
    let error = ensure(&dir).await.unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert!(error.to_string().contains("is not a directory"), "{error}");
}

/// 别人的目录：用 root 拥有的 `/` 代表（root 运行测试时属主相同，跳过）。
#[cfg(unix)]
#[tokio::test]
async fn a_directory_of_another_user_is_refused() {
    if uid() == 0 {
        return;
    }
    let error = ensure(Path::new("/")).await.unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert!(
        error.to_string().starts_with(&format!(
            "Temp directory / is owned by uid 0, expected {}.",
            uid()
        )),
        "{error}"
    );
}
