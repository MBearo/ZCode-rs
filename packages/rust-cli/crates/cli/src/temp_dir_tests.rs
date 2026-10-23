//! 临时目录的算法（修复线 F58）：照 Node `os.tmpdir()`。

use super::*;

fn env(pairs: &[(&str, &str)]) -> EnvSnapshot {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

/// Unix：`TMPDIR` → `TMP` → `TEMP` → `/tmp`，空值跳过，去掉一个末尾的 `/`。
/// 以前入口用 `std::env::temp_dir()`：只看 `TMPDIR`，`TMPDIR=` 得到空路径；外部编辑器另按 TMPDIR → TEMP → TMP。
#[test]
fn unix_follows_node() {
    let dir = |pairs: &[(&str, &str)]| node_tmpdir(&env(pairs), false);
    assert_eq!(dir(&[]), PathBuf::from("/tmp"));
    assert_eq!(dir(&[("TMPDIR", "/var/t/")]), PathBuf::from("/var/t"));
    assert_eq!(dir(&[("TMPDIR", "/")]), PathBuf::from("/"));
    assert_eq!(dir(&[("TMPDIR", ""), ("TMP", "/x")]), PathBuf::from("/x"));
    assert_eq!(dir(&[("TEMP", "/b"), ("TMP", "/a")]), PathBuf::from("/a"));
    assert_eq!(dir(&[("TEMP", "/b")]), PathBuf::from("/b"));
}

/// Windows：`TEMP` → `TMP` → `%SystemRoot%\temp`（再没有时 `windir`），去掉末尾的 `\`（`C:\` 除外）。
#[test]
fn windows_follows_node() {
    let dir = |pairs: &[(&str, &str)]| node_tmpdir(&env(pairs), true);
    assert_eq!(
        dir(&[
            ("TEMP", r"C:\Users\dev\AppData\Local\Temp\"),
            ("TMP", r"D:\t")
        ]),
        PathBuf::from(r"C:\Users\dev\AppData\Local\Temp")
    );
    assert_eq!(dir(&[("TMP", r"D:\t")]), PathBuf::from(r"D:\t"));
    assert_eq!(dir(&[("TEMP", r"C:\")]), PathBuf::from(r"C:\"));
    assert_eq!(
        dir(&[("SystemRoot", r"C:\Windows")]),
        PathBuf::from(r"C:\Windows\temp")
    );
    assert_eq!(dir(&[("windir", r"C:\Win")]), PathBuf::from(r"C:\Win\temp"));
}
