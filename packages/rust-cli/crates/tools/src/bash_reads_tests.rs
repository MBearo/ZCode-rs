//! Bash 读文件记为已读（tools-files.md §4.2）：认哪些命令、记下的形状。

use std::sync::Arc;

use rust_cli_kernel_api::env::{EnvFacts, EnvPath};
use rust_cli_kernel_api::testing::{MemoryEnvironment, ScriptedInteractions};

use super::{Range, ReadCommand, recognize, record, shape};
use crate::file_state::ReadFileState;
use crate::test_more::Ctx;

fn read(path: &str, range: Range) -> ReadCommand {
    ReadCommand {
        path: path.to_owned(),
        range,
        search: false,
    }
}

#[test]
fn whole_file_readers_are_recognized() {
    for command in [
        "cat a.txt",
        "cat -n a.txt",
        "nl a.txt",
        "bat --plain a.txt",
        "batcat -p a.txt",
    ] {
        assert_eq!(
            recognize(command),
            [read("a.txt", Range::Whole)],
            "{command}"
        );
    }
    assert_eq!(
        recognize("cat 'my file.txt'"),
        [read("my file.txt", Range::Whole)]
    );
    assert_eq!(
        recognize("LC_ALL=C cat a.txt"),
        [read("a.txt", Range::Whole)]
    );
    for command in [
        "cat -v a.txt",
        "cat a.txt b.txt",
        "cat -",
        "cat",
        "nl -ba a.txt",
    ] {
        assert!(recognize(command).is_empty(), "{command}");
    }
}

#[test]
fn head_tail_and_sed_give_line_ranges() {
    let lines = |start, end| Range::Lines { start, end };
    assert_eq!(recognize("head a.txt"), [read("a.txt", lines(1, 10))]);
    for command in [
        "head -n 3 a.txt",
        "head --lines 3 a.txt",
        "head --lines=3 a.txt",
        "head -n3 a.txt",
        "head -3 a.txt",
    ] {
        assert_eq!(
            recognize(command),
            [read("a.txt", lines(1, 3))],
            "{command}"
        );
    }
    assert_eq!(
        recognize("tail -n 2 a.txt"),
        [read("a.txt", Range::Tail { count: 2 })]
    );
    assert_eq!(
        recognize("sed -n '2,4p' a.txt"),
        [read("a.txt", lines(2, 4))]
    );
    assert_eq!(
        recognize("sed --quiet 5p a.txt"),
        [read("a.txt", lines(5, 5))]
    );
    for command in [
        "head -n 0 a.txt",
        "head -c 10 a.txt",
        "tail -n +2 a.txt",
        "head -",
        "sed '2,4p' a.txt",
        "sed -n -i '2p' a.txt",
        "sed -n -e 2p a.txt",
        "sed -n 's/a/b/p' a.txt",
        "sed -n 2p",
    ] {
        assert!(recognize(command).is_empty(), "{command}");
    }
}

#[test]
fn single_file_searches_need_a_plain_file() {
    let search = |path: &str| ReadCommand {
        path: path.to_owned(),
        range: Range::Whole,
        search: true,
    };
    for command in [
        "grep -n foo a.txt",
        "grep -A 2 foo a.txt",
        "grep -C3 --color=never foo a.txt",
        "rg -i foo a.txt",
        "rg --context 2 foo a.txt",
    ] {
        assert_eq!(recognize(command), [search("a.txt")], "{command}");
    }
    for command in [
        "grep foo",
        "grep foo a.txt b.txt",
        "grep -r foo .",
        "grep foo '*.txt'",
        "rg --context=x foo a.txt",
        "grep -n foo a.txt && cat b.txt",
    ] {
        assert!(recognize(command).is_empty(), "{command}");
    }
}

/// 多段命令：每段都要认得或是 `echo` 一类；管道与重定向（引号里也算）、展开都不认。
#[test]
fn compound_commands_need_every_segment() {
    assert_eq!(
        recognize("cat a.txt && echo --- && head -2 b.txt"),
        [
            read("a.txt", Range::Whole),
            read("b.txt", Range::Lines { start: 1, end: 2 })
        ]
    );
    assert_eq!(recognize("true; cat a.txt"), [read("a.txt", Range::Whole)]);
    for command in [
        "cd sub && cat a.txt",
        "cat a.txt | head",
        "cat a.txt > b.txt",
        "cat 'a>b.txt'",
        "cat $FILE",
        "cat $(ls)",
        ": ; cat a.txt",
    ] {
        assert!(recognize(command).is_empty(), "{command}");
    }
}

#[test]
fn shapes_follow_the_requested_lines() {
    let text = "a\nb\nc\n";
    assert_eq!(
        shape(text, &Range::Whole),
        Some((text.to_owned(), None, None))
    );
    assert_eq!(
        shape(text, &Range::Lines { start: 1, end: 10 }),
        Some(("a\nb\nc\n".to_owned(), Some(1), Some(10)))
    );
    assert_eq!(
        shape(text, &Range::Lines { start: 2, end: 2 }),
        Some(("b".to_owned(), Some(2), Some(1)))
    );
    assert_eq!(shape(text, &Range::Lines { start: 9, end: 9 }), None);
    assert_eq!(
        shape(text, &Range::Tail { count: 2 }),
        Some(("b\nc".to_owned(), Some(2), Some(2)))
    );
    assert_eq!(
        shape(text, &Range::Tail { count: 10 }),
        Some(("a\nb\nc".to_owned(), Some(1), Some(3)))
    );
    assert_eq!(shape("", &Range::Tail { count: 1 }), None);
}

fn env() -> Arc<MemoryEnvironment> {
    let env = MemoryEnvironment::new(EnvFacts {
        cwd: EnvPath::new("/Users/dev/work"),
        platform: "darwin".to_owned(),
        os_version: "Darwin 24".to_owned(),
        shell: "zsh".to_owned(),
        is_git_repo: false,
        additional_directories: Vec::new(),
        home: Some(EnvPath::new("/Users/dev")),
    })
    .with_file("/Users/dev/work/a.txt", "one\ntwo\n")
    .with_file("/Users/dev/work/sub/b.txt", "x\ny\nz\n");
    Arc::new(env)
}

/// 内容取文件本身；相对路径按会话的当前目录；已有的记录不覆盖；grep 要退出码 0、记录不算在上下文里。
#[tokio::test]
async fn recorded_files_come_from_disk() {
    let env = env();
    env.set_shell_cwd("/Users/dev/work/sub");
    let mut ctx = Ctx::new(Arc::new(ScriptedInteractions::new([])));
    ctx.set_env(env.clone());
    let context = ctx.build();
    let state = ReadFileState::default();
    record(
        &state,
        &context,
        "cat ../a.txt && sed -n 2,3p b.txt",
        (0, false),
    )
    .await;
    let a = state
        .get(&context.session, "/Users/dev/work/a.txt")
        .unwrap();
    assert_eq!(
        (a.content.as_str(), a.offset, a.limit),
        ("one\ntwo\n", None, None)
    );
    assert!(a.is_in_context());
    let b = state
        .get(&context.session, "/Users/dev/work/sub/b.txt")
        .unwrap();
    assert_eq!(
        (b.content.as_str(), b.offset, b.limit),
        ("y\nz", Some(2), Some(2))
    );

    env.set_file("/Users/dev/work/a.txt", "changed\n");
    record(&state, &context, "cat ../a.txt", (0, true)).await;
    assert_eq!(
        state
            .get(&context.session, "/Users/dev/work/a.txt")
            .unwrap()
            .content,
        "one\ntwo\n"
    );

    let other = ReadFileState::default();
    record(&other, &context, "grep -n x b.txt", (1, false)).await;
    assert!(
        other
            .get(&context.session, "/Users/dev/work/sub/b.txt")
            .is_none()
    );
    record(&other, &context, "grep -n x b.txt", (0, false)).await;
    assert!(
        other
            .get(&context.session, "/Users/dev/work/sub/b.txt")
            .unwrap()
            .not_in_context
    );
    record(&other, &context, "cat ../a.txt", (0, true)).await;
    assert!(
        other
            .get(&context.session, "/Users/dev/work/a.txt")
            .unwrap()
            .not_in_context
    );
    record(&other, &context, "cat missing.txt", (0, false)).await;
    assert!(
        other
            .get(&context.session, "/Users/dev/work/sub/missing.txt")
            .is_none()
    );
}
