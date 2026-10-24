//! `Search`：按修改时间排序的 glob（含隐藏与被忽略的文件）、grep 的三种模式与拒绝、遵守忽略规则的文件清单。

use std::fs::File;
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use rust_cli_env_local::LocalEnvironment;
use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::env_files::{
    GlobFiles, GrepLineKind, GrepMode, GrepOutcome, GrepSearch, ListFiles,
};

use crate::contract;
use crate::support::{Kind, machine, workspace};

fn write(root: &Path, name: &str, content: &str, mtime: u64) {
    let path = root.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, content).unwrap();
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

async fn glob_sorts_by_modification_time_and_includes_hidden_and_ignored_files(kind: Kind) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "src/b.ts", "b", 10);
    write(root, "src/a.ts", "a", 20);
    write(root, ".hidden/h.ts", "h", 30);
    write(root, ".gitignore", "ignored.ts\n", 40);
    write(root, "ignored.ts", "i", 50);
    write(root, "src/c.js", "c", 60);
    let env = machine(kind, LocalEnvironment::new(None)).await;
    let request = GlobFiles {
        root: EnvPath::new(root.to_string_lossy()),
        pattern: "**/*.ts".to_owned(),
        limit: 3,
        hidden: true,
        respect_ignore: false,
    };
    let found = env.search().glob(&workspace(root), request).await.unwrap();
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

async fn grep_reports_files_counts_and_lines_with_context(kind: Kind) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "one.txt", "foo\nbar\nfoo bar\n", 1);
    write(root, "two.js", "nothing\n", 2);
    write(root, "bin.dat", "foo\0binary\n", 3);
    let env = machine(kind, LocalEnvironment::new(None)).await;
    let grep = |request| {
        let search = env.search().clone();
        async move { search.grep(&workspace(root), request).await.unwrap() }
    };
    let GrepOutcome::Files { files } = grep(search(root, "foo", GrepMode::FilesWithMatches)).await
    else {
        panic!("files");
    };
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].mtime_ms, 1000);
    let GrepOutcome::Counts { counts } = grep(search(root, "foo", GrepMode::Count)).await else {
        panic!("counts");
    };
    assert_eq!(counts[0].count, 2);
    let mode = GrepMode::Content {
        before: 0,
        after: 1,
    };
    let GrepOutcome::Content { files } = grep(search(root, "^foo$", mode)).await else {
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
    let rejected = grep(search(root, "(", GrepMode::FilesWithMatches)).await;
    assert_eq!(
        rejected,
        GrepOutcome::Rejected {
            message: "regex parse error:\n    (?:()\n    ^\nerror: unclosed group".to_owned()
        }
    );
}

async fn list_files_follows_the_ignore_rules_and_skips_vcs_directories(kind: Kind) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join(".git")).unwrap();
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
    let env = machine(kind, LocalEnvironment::new(None)).await;
    let request = ListFiles {
        cwd: EnvPath::new(root.to_string_lossy()),
    };
    let files: Vec<String> = env
        .search()
        .list_files(&workspace(root), request)
        .await
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

contract!(
    glob_sorts_by_modification_time_and_includes_hidden_and_ignored_files,
    grep_reports_files_counts_and_lines_with_context,
    list_files_follows_the_ignore_rules_and_skips_vcs_directories,
);
