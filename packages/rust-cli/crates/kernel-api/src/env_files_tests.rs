//! 文件与搜索 DTO 的序列化往返（docs/specs/rust-cli/README.md §6 规则 8）。

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::*;
use crate::env::{EnvPath, FileVersion, WritePrecondition};

fn round_trip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(value: &T) {
    let text = serde_json::to_string(value).unwrap();
    let back: T = serde_json::from_str(&text).unwrap();
    assert_eq!(&back, value, "{text}");
}

fn path(value: &str) -> EnvPath {
    EnvPath::new(value)
}

#[test]
fn read_side_requests_and_results_round_trip() {
    round_trip(&PathFacts {
        home: Some(path("/home/dev")),
        style: PathStyle::Posix,
    });
    round_trip(&Some(FileStat {
        kind: FileKind::File,
        size: 3,
        mtime_ms: 1_767_225_601_000,
        version: FileVersion::new("v1"),
    }));
    round_trip(&vec![DirEntry {
        name: "a.txt".to_owned(),
        kind: FileKind::Other,
    }]);
    round_trip(&ReadText {
        path: path("/w/a.txt"),
        start_line: 4,
        max_lines: Some(3),
        max_bytes: None,
    });
    round_trip(&TextRead::Text(TextSlice {
        content: "a\nb".to_owned(),
        line_count: 2,
        total_lines: 3,
        total_bytes: 4,
        read_bytes: 3,
        mtime_ms: 1,
        version: FileVersion::new("v1"),
    }));
    round_trip(&TextRead::TooLarge { size: 300_000 });
    round_trip(&TextRead::Directory);
}

#[test]
fn writes_round_trip() {
    round_trip(&PutFile {
        path: path("C:\\w\\a.txt"),
        bytes: vec![0xff, 0xfe, 0],
        precondition: WritePrecondition::Version(FileVersion::new("v1")),
        create_parents: true,
    });
    round_trip(&FileWritten {
        version: FileVersion::new("v2"),
        mtime_ms: 2,
        created: false,
    });
}

#[test]
fn search_requests_and_results_round_trip() {
    round_trip(&GlobFiles {
        root: path("/w"),
        pattern: "**/*.ts".to_owned(),
        limit: 100,
        hidden: true,
        respect_ignore: false,
    });
    round_trip(&GlobMatches {
        files: vec![path("/w/a.ts")],
        total: 101,
    });
    round_trip(&GrepSearch {
        root: path("/w"),
        pattern: "foo".to_owned(),
        case_insensitive: true,
        multiline: false,
        globs: vec!["!.git".to_owned()],
        file_type: Some("js".to_owned()),
        hidden: true,
        mode: GrepMode::Content {
            before: 1,
            after: 2,
        },
    });
    round_trip(&GrepOutcome::Content {
        files: vec![GrepFileLines {
            path: path("/w/a.txt"),
            lines: vec![GrepLine {
                kind: GrepLineKind::Match,
                line_number: 1,
                text: "foo\n".to_owned(),
                submatches: vec![GrepSubmatch {
                    start: 0,
                    text: "foo".to_owned(),
                }],
            }],
            binary_offset: Some(9),
        }],
    });
    round_trip(&GrepOutcome::Files {
        files: vec![GrepFileHit {
            path: path("/w/a.txt"),
            mtime_ms: 5,
        }],
    });
    round_trip(&GrepOutcome::Counts {
        counts: vec![GrepCount {
            path: path("/w/a.txt"),
            count: 2,
        }],
    });
    round_trip(&GrepOutcome::Rejected {
        message: "regex parse error".to_owned(),
    });
}

#[test]
fn version_control_directories_keep_their_order() {
    // Grep 排除的版本库目录：值与顺序固定。
    assert_eq!(VCS_DIRS, [".git", ".svn", ".hg", ".bzr", ".jj", ".sl"]);
}
