//! 后台命令输出文件末尾的终态行（shell-permissions.md §2.2）：`mark_exit` 返回时已经在文件里；
//! Windows 上 cwd 文件的 Git Bash 写法（§2.1，工作线 F57）。
#![allow(clippy::disallowed_methods, reason = "测试准备输出文件并同步读回")]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rust_cli_kernel_api::env::Outcome;
use rust_cli_kernel_api::env_files::PathStyle;

use super::*;

fn plan(dir: &Path) -> Plan {
    Plan {
        dir: dir.to_path_buf(),
        output: dir.join("b1.output"),
        errors: None,
        cwd_file: dir.join("b1.cwd"),
        cwd: dir.to_path_buf(),
        origin: dir.to_path_buf(),
        bash: PathBuf::from("/bin/bash"),
        env: BTreeMap::new(),
        prelude: None,
    }
}

/// 修复线 F35：tokio 的 `File::write_all` 只把数据交给后台线程就返回，原来没有 flush，`mark_exit` 返回时这一行多半
/// 还没写进文件（插桩：约 98% 的调用），终态先发出，读输出的一方偶发看不到 `[killed]` / `[exited with code N]`。
/// 每次返回后立即同步读文件；修前 200 次运行全部失败（多在第一次就缺这一行）。
#[tokio::test]
async fn the_exit_line_is_on_disk_when_mark_exit_returns() {
    let dir = tempfile::tempdir().unwrap();
    let plan = plan(dir.path());
    let cases = [
        (Outcome::Completed, Some(0), "\n[exited with code 0]\n"),
        (Outcome::Cancelled, Some(137), "\n[killed]\n"),
    ];
    for round in 0..10 {
        for (outcome, exit, line) in &cases {
            let before = format!("output {round}\n");
            std::fs::write(&plan.output, &before).unwrap();
            mark_exit(&plan, outcome, *exit).await;
            let written = std::fs::read_to_string(&plan.output).unwrap();
            assert_eq!(written, format!("{before}{line}"), "round {round}");
        }
    }
}

/// 工作线 F57：Windows 上写进脚本的 cwd 文件路径换成 Git Bash 写法（Node `cwd-capture.ts` 的
/// `windowsPathToGitBashPath`），POSIX 原样。以前写 `C:\…` 原样。
#[test]
fn the_cwd_file_is_spelled_for_git_bash_on_windows() {
    let file = Path::new(r"C:\Users\dev\AppData\Local\Temp\zcode\s1\tasks\b1.cwd");
    assert_eq!(
        cwd_file_spelling(file, PathStyle::Windows),
        "/c/Users/dev/AppData/Local/Temp/zcode/s1/tasks/b1.cwd"
    );
    assert_eq!(
        cwd_file_spelling(Path::new(r"\\server\share\b1.cwd"), PathStyle::Windows),
        "//server/share/b1.cwd"
    );
    assert_eq!(
        cwd_file_spelling(Path::new("/t/b1.cwd"), PathStyle::Posix),
        "/t/b1.cwd"
    );
}

/// 工作线 F57：读回的 `pwd -P` 照 Node `gitBashPathToWindowsPath`（`contracts/src/path/git-bash.ts`）换回；
/// 期望值是用 Node 的这段函数实测的。以前只认 `/c/…`，`/cygdrive/c/…` 与 `//server/…` 原样当路径。
#[test]
fn git_bash_cwds_are_read_back_like_node() {
    for (text, expected) in [
        ("/c/Users/dev", r"C:\Users\dev"),
        ("/c", r"C:\"),
        ("/c/", r"C:\"),
        ("/c//x", r"C:\x"),
        ("/D/work//x", r"D:\work\\x"),
        ("//server/share/x", r"\\server\share\x"),
        ("/cygdrive/c/x", r"C:\x"),
        ("/cygdrive/d", r"D:\"),
        ("/usr/bin", r"\usr\bin"),
        ("/cd/x", r"\cd\x"),
        ("relative/x", r"relative\x"),
    ] {
        assert_eq!(
            native_cwd(text, PathStyle::Windows),
            PathBuf::from(expected),
            "{text}"
        );
    }
    // POSIX 原样。
    assert_eq!(
        native_cwd("/c/Users/dev", PathStyle::Posix),
        PathBuf::from("/c/Users/dev")
    );
}
