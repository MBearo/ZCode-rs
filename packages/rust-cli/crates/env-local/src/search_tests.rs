#![expect(clippy::disallowed_methods, reason = "测试直接准备工作区夹具")]

use std::fs::{self, File};
use std::time::{Duration, UNIX_EPOCH};

use super::*;

fn write(root: &Path, name: &str, content: &str, mtime: u64) {
    let path = root.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, content).unwrap();
    File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(UNIX_EPOCH + Duration::from_secs(mtime))
        .unwrap();
}

fn names(root: &Path, files: &[EnvPath]) -> Vec<String> {
    let prefix = format!("{}{}", root.display(), std::path::MAIN_SEPARATOR);
    files
        .iter()
        .map(|file| file.as_str().trim_start_matches(&prefix).replace('\\', "/"))
        .collect()
}

#[test]
fn glob_sorts_by_modification_time_and_includes_hidden_and_ignored_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "src/b.ts", "b", 10);
    write(root, "src/a.ts", "a", 20);
    write(root, ".hidden/h.ts", "h", 30);
    write(root, ".gitignore", "ignored.ts\n", 40);
    write(root, "ignored.ts", "i", 50);
    write(root, "src/c.js", "c", 60);
    let request = GlobFiles {
        root: EnvPath::new(root.to_string_lossy()),
        pattern: "**/*.ts".to_owned(),
        limit: 3,
        hidden: true,
        respect_ignore: false,
    };
    let found = glob(&request, &CancellationToken::new()).unwrap();
    assert_eq!(found.total, 4);
    assert_eq!(
        names(root, &found.files),
        ["src/b.ts", "src/a.ts", ".hidden/h.ts"]
    );
}

fn search(root: &Path, pattern: &str, mode: GrepMode) -> GrepSearch {
    GrepSearch {
        root: EnvPath::new(root.to_string_lossy()),
        pattern: pattern.to_owned(),
        case_insensitive: false,
        multiline: false,
        globs: vec!["!.git".to_owned()],
        file_type: None,
        hidden: true,
        mode,
    }
}

#[test]
fn grep_reports_files_counts_and_lines_with_context() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "one.txt", "foo\nbar\nfoo bar\n", 1);
    write(root, "two.js", "nothing\n", 2);
    write(root, "bin.dat", "foo\0binary\n", 3);
    let GrepOutcome::Files { files } = grep(
        &search(root, "foo", GrepMode::FilesWithMatches),
        &CancellationToken::new(),
    )
    .unwrap() else {
        panic!("files");
    };
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].mtime_ms, 1000);
    let GrepOutcome::Counts { counts } = grep(
        &search(root, "foo", GrepMode::Count),
        &CancellationToken::new(),
    )
    .unwrap() else {
        panic!("counts");
    };
    assert_eq!(counts[0].count, 2);
    let mode = GrepMode::Content {
        before: 0,
        after: 1,
    };
    let GrepOutcome::Content { files } =
        grep(&search(root, "^foo$", mode), &CancellationToken::new()).unwrap()
    else {
        panic!("content");
    };
    let lines = &files[0].lines;
    assert_eq!(lines.len(), 2);
    assert_eq!(
        (lines[0].kind, lines[0].line_number),
        (GrepLineKind::Match, 1)
    );
    assert_eq!(lines[0].text, "foo\n");
    assert_eq!(lines[0].submatches[0].text, "foo");
    assert_eq!(
        (lines[1].kind, lines[1].line_number),
        (GrepLineKind::Context, 2)
    );
}

#[test]
fn grep_rejects_bad_patterns_and_unknown_types_with_rg_messages() {
    let dir = tempfile::tempdir().unwrap();
    let outcome = grep(
        &search(dir.path(), "(", GrepMode::FilesWithMatches),
        &CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(
        outcome,
        GrepOutcome::Rejected {
            message: "regex parse error:\n    (?:()\n    ^\nerror: unclosed group".to_owned()
        }
    );
    let mut request = search(dir.path(), "x", GrepMode::FilesWithMatches);
    request.file_type = Some("nope".to_owned());
    assert_eq!(
        grep(&request, &CancellationToken::new()).unwrap(),
        GrepOutcome::Rejected {
            message: "unrecognized file type: nope".to_owned()
        }
    );
}

/// `@` 补全的清单（tui-input.md §1.2）：遵守 .gitignore / .ignore / .rgignore，含隐藏文件，跳过版本库目录。
#[test]
fn list_files_follows_the_ignore_rules_and_skips_vcs_directories() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join(".git")).unwrap();
    write(root, ".git/config", "x", 1);
    write(root, ".gitignore", "target/\n", 2);
    write(root, ".ignore", "secret.txt\n", 3);
    write(root, ".rgignore", "*.log\n", 4);
    write(root, "src/main.rs", "m", 5);
    write(root, ".env.example", "e", 6);
    write(root, "target/out.bin", "o", 7);
    write(root, "secret.txt", "s", 8);
    write(root, "run.log", "l", 9);
    write(root, ".hg/store", "h", 10);
    let request = ListFiles {
        cwd: EnvPath::new(root.to_string_lossy()),
    };
    let files: Vec<String> = list_files(&request, &CancellationToken::new())
        .unwrap()
        .files
        .into_iter()
        .map(|file| file.replace('\\', "/"))
        .collect();
    assert_eq!(
        files,
        [
            ".env.example",
            ".gitignore",
            ".ignore",
            ".rgignore",
            "src/main.rs"
        ]
    );
}

/// R5-13：取消后遍历在下一个条目处停下（三种遍历都一样）。
#[test]
fn a_cancelled_walk_stops_at_the_next_entry() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for index in 0..20 {
        write(root, &format!("d{index}/f.ts"), "foo", 10);
    }
    let stop = CancellationToken::new();
    stop.cancel();
    let request = GlobFiles {
        root: EnvPath::new(root.to_string_lossy()),
        pattern: "**/*.ts".to_owned(),
        limit: 100,
        hidden: true,
        respect_ignore: false,
    };
    assert_eq!(glob(&request, &stop).unwrap().total, 0);
    let GrepOutcome::Files { files } =
        grep(&search(root, "foo", GrepMode::FilesWithMatches), &stop).unwrap()
    else {
        panic!("files mode");
    };
    assert!(files.is_empty());
    let listed = list_files(
        &ListFiles {
            cwd: EnvPath::new(root.to_string_lossy()),
        },
        &stop,
    )
    .unwrap();
    assert!(listed.files.is_empty());
    // 没有取消时同样的遍历能找到全部 20 个文件。
    assert_eq!(glob(&request, &CancellationToken::new()).unwrap().total, 20);
}

/// R5-13：丢下 `run_walk` 的 future（调用方被取消）时，阻塞线程里的工作收到取消。
#[tokio::test(flavor = "multi_thread")]
async fn dropping_the_walk_future_cancels_the_blocking_work() {
    let (started, on_start) = std::sync::mpsc::channel();
    let (stopped, on_stop) = std::sync::mpsc::channel();
    let walk = crate::helpers::run_walk(move |stop| {
        let _ = started.send(());
        while !stop.is_cancelled() {
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = stopped.send(());
        Ok(())
    });
    let task = rust_cli_base::owned(walk);
    on_start.recv_timeout(Duration::from_secs(5)).unwrap();
    drop(task);
    // 期限只用于让失败的测试结束。
    on_stop.recv_timeout(Duration::from_secs(5)).unwrap();
}
