use rust_cli_kernel_api::env_rewind::content_hash;

use super::*;

fn write(path: &str, before: Option<&str>, after: &str) -> Artifact {
    Artifact {
        files: vec![ArtifactFile {
            path: path.into(),
            existed_before: before.is_some(),
            before_content: before.map(str::to_owned),
            after_content: Some(after.into()),
        }],
    }
}

/// 同一路径多次写入：期望最后一次写入后的内容，恢复成最早一次写入前的内容；新建的文件删除。
#[test]
fn plans_collapse_writes_per_path_in_reverse() {
    let artifacts = [
        write("/a", Some("v0"), "v1"),
        write("/new", None, "n1"),
        write("/a", Some("v1"), "v2"),
    ];
    let files = plan(&artifacts).unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].path.as_str(), "/a");
    assert_eq!(files[0].expected, Some(content_hash(b"v2")));
    assert_eq!(files[0].restore.as_deref(), Some(b"v0".as_slice()));
    assert_eq!(files[1].path.as_str(), "/new");
    assert_eq!(files[1].restore, None);
}

/// 两次写入之间文件被别的方式改过（接不上）：冲突。
#[test]
fn a_gap_between_writes_is_a_conflict() {
    let artifacts = [write("/a", Some("v0"), "v1"), write("/a", Some("vX"), "v2")];
    assert_eq!(
        plan(&artifacts).unwrap_err(),
        "Cannot restore code: /a was changed after ZCode edited it"
    );
}

#[test]
fn changes_count_lines_from_first_before_to_last_after() {
    let artifacts = [
        write("/b", Some("1\n2\n3\n"), "1\nx\n3\n"),
        write("/b", Some("1\nx\n3\n"), "1\nx\n3\n4\n"),
    ];
    let changes = changes(&artifacts);
    assert_eq!(changes.len(), 1);
    assert_eq!((changes[0].additions, changes[0].deletions), (2, 1));
}
