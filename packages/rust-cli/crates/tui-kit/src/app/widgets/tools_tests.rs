//! 工具调用的措辞（docs/puffin-tui.md §4.2「工具调用」；docs/specs/rust-cli/tui-kit.md §4.4）：名字表、对象、行尾的结果、
//! 进行中的末尾、挂在下面的东西、一段工作的 `Tally` 与读的计数，入参是中性的 `Call`。Puffin 的 tools_tests 经它的条目
//! 映射测同样的规则；这里每个函数一个有代表性的输入，另有随 `fit` 搬来的数与时间的写法。
use serde_json::{Value, json};

use crate::app::widgets::fit;
use crate::app::widgets::tools::{
    self, Call, Changes, DiffRow, Facts, Ink, NumberedLine, Sign, Status, Under, Where, no_todos,
};

fn here() -> Where {
    Where {
        cwd: "/Users/dev/project".to_owned(),
        home: Some("/Users/dev".to_owned()),
    }
}

/// 一个结束了的调用：`tool` 的名字与事实，`input` 与 `output` 是它的。
fn call<'a>(id: &'a str, tool: &'a str, input: &'a Value, output: &'a str) -> Call<'a> {
    let mut f = Facts::named(tool, input);
    f.output = output;
    Call {
        id,
        name: tool,
        summary: "",
        status: Status::Done,
        response: "",
        ended: true,
        facts: Some(f),
    }
}

fn parts(c: &Call<'_>) -> Vec<(String, Ink)> {
    tools::outcome(c, &no_todos)
        .into_iter()
        .map(|p| (p.text, p.ink))
        .collect()
}

#[test]
fn the_name_table_gives_plain_names_and_an_mcp_tool_goes_by_its_own_name() {
    let none = json!({});
    let names: Vec<String> = ["Grep", "Glob", "WebFetch", "TodoWrite", "TaskStop", "Bash"]
        .iter()
        .map(|t| tools::name(&call("t", t, &none, "")))
        .collect();
    assert_eq!(
        names,
        ["Search", "Find", "Fetch", "Todos", "Stop", "Bash"].map(str::to_owned)
    );
    assert_eq!(
        tools::name(&call("t", "mcp__github__create_issue", &none, "")),
        "create_issue"
    );
    // 没有事实的调用（排队的）用投影给的名字与摘要。
    let queued = Call {
        id: "q",
        name: "Read",
        summary: "a.ts\nmore",
        status: Status::Queued,
        response: "",
        ended: false,
        facts: None,
    };
    assert_eq!(tools::name(&queued), "Read");
    assert_eq!(
        tools::target(&queued, &here(), &|_| None, &no_todos),
        "a.ts …"
    );
}

#[test]
fn the_target_writes_paths_short_and_drops_a_leading_cd_into_the_folder() {
    let read = json!({"file_path": "/Users/dev/project/src/a.ts"});
    let home = json!({"file_path": "/Users/dev/notes.md"});
    let grep = json!({"pattern": "TODO", "path": "/Users/dev/project/src"});
    let bash = json!({"command": "cd /Users/dev/project && cargo test /Users/dev/project/x"});
    let t = |input: &Value, tool: &str| {
        tools::target(&call("t", tool, input, ""), &here(), &|_| None, &no_todos)
    };
    assert_eq!(t(&read, "Read"), "src/a.ts");
    assert_eq!(t(&home, "Read"), "~/notes.md");
    assert_eq!(t(&grep, "Grep"), "TODO in src");
    assert_eq!(t(&bash, "Bash"), "cargo test x");
}

#[test]
fn the_outcome_at_the_row_end_counts_what_came_back_and_a_failure_is_in_err() {
    let none = json!({});
    let a = json!({"file_path": "a"});
    let read = call("t", "Read", &a, "1\tone\n2\ttwo\n3\tthree");
    assert_eq!(parts(&read), vec![("3 lines".to_owned(), Ink::Fg3)]);
    let mut bash = call("t", "Bash", &none, "Exit code 1\nboom");
    if let Some(f) = bash.facts.as_mut() {
        f.started_at = 1_000;
        f.ended_at = 4_200;
    }
    assert!(tools::failed(&bash));
    assert_eq!(tools::status(&bash), "error");
    assert_eq!(
        parts(&bash),
        vec![
            ("exit 1".to_owned(), Ink::Err),
            (" · 3.2s".to_owned(), Ink::Fg3)
        ]
    );
    let lines = [NumberedLine {
        sign: Sign::Add,
        text: "x".to_owned(),
        line: 1,
    }];
    let mut edit = call("t", "Edit", &none, "");
    if let Some(f) = edit.facts.as_mut() {
        f.diff = Some(Changes {
            added: 6,
            removed: 3,
            lines: &lines,
        });
    }
    assert_eq!(
        parts(&edit),
        vec![
            ("+6".to_owned(), Ink::Ok),
            (" ".to_owned(), Ink::Fg3),
            ("−3".to_owned(), Ink::Err)
        ]
    );
    let mut stopped = call("t", "Grep", &none, "");
    stopped.status = Status::Cancelled;
    assert_eq!(parts(&stopped), vec![("stopped".to_owned(), Ink::Fg3)]);
}

#[test]
fn a_running_command_says_its_seconds_after_three_and_its_tail_is_the_last_three_lines() {
    let input = json!({"command": "make"});
    let mut c = call("t", "Bash", &input, "");
    c.ended = false;
    c.status = Status::Running;
    if let Some(f) = c.facts.as_mut() {
        f.started_at = 10_000;
        f.preview = "a\nb\n\nc\nd\n";
    }
    assert!(tools::live(&c, 12_500.0).is_empty());
    let live = tools::live(&c, 14_200.0);
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].text, "· 4s");
    assert_eq!(
        tools::under(&c, false, &here(), &no_todos),
        Under::Tail(vec!["b".to_owned(), "c".to_owned(), "d".to_owned()])
    );
}

#[test]
fn an_ended_edit_hangs_its_numbered_diff_and_a_failure_its_reason() {
    let none = json!({});
    let lines = [
        NumberedLine {
            sign: Sign::Remove,
            text: "old\tx".to_owned(),
            line: 40,
        },
        NumberedLine {
            sign: Sign::Add,
            text: "new".to_owned(),
            line: 40,
        },
    ];
    let mut edit = call("t", "Edit", &none, "");
    if let Some(f) = edit.facts.as_mut() {
        f.diff = Some(Changes {
            added: 1,
            removed: 1,
            lines: &lines,
        });
    }
    assert_eq!(
        tools::under(&edit, true, &here(), &no_todos),
        Under::Diff(vec![
            DiffRow {
                n: 40,
                sign: '-',
                text: "old    x".to_owned()
            },
            DiffRow {
                n: 40,
                sign: '+',
                text: "new".to_owned()
            },
        ])
    );
    let mut failed = call("t", "Read", &none, "");
    failed.status = Status::Error;
    if let Some(f) = failed.facts.as_mut() {
        f.error = "File not found\n\n";
    }
    assert_eq!(
        tools::under(&failed, false, &here(), &no_todos),
        Under::Reason(vec!["File not found".to_owned()])
    );
}

#[test]
fn a_tally_counts_the_run_by_kind_and_names_the_failures() {
    let a = json!({"file_path": "/Users/dev/project/a.ts"});
    let b = json!({"file_path": "/Users/dev/project/b.ts"});
    let cmd = json!({"command": "ls"});
    let none = json!({});
    let mut broken = call("t5", "Grep", &none, "");
    broken.status = Status::Error;
    let calls = [
        call("t1", "Read", &a, ""),
        call("t2", "Read", &a, ""),
        call("t3", "Read", &b, ""),
        call("t4", "Bash", &cmd, ""),
        broken,
        call("t6", "ToolSearch", &none, ""),
    ];
    let t = tools::tally(&calls, &here(), &no_todos);
    assert_eq!(t.files, 2, "a file read twice counts once");
    assert_eq!(t.commands, 1);
    assert_eq!(t.failed, vec!["Search".to_owned()]);
    assert_eq!(t.ok, 4, "plumbing is not counted");
    assert_eq!(tools::counts(&calls[..3]), "2 files");
}

#[test]
fn a_subagent_s_current_call_reads_like_a_row_label() {
    let input = json!({"file_path": "/Users/dev/project/src/main.rs"});
    let cur = tools::Current {
        tool: "Read",
        input: &input,
    };
    assert_eq!(tools::call_label(cur, &here()), "Read src/main.rs");
    assert_eq!(
        tools::call_parts(cur, &here()),
        ("Read".to_owned(), "src/main.rs".to_owned())
    );
}

#[test]
fn counts_tokens_and_times_are_written_the_scala_way() {
    assert_eq!(
        fit::fixed(0.25, 1),
        "0.3",
        "half rounds up on the shortest decimal"
    );
    assert_eq!(fit::tokens(950.0), "950");
    assert_eq!(fit::tokens(52_300.0), "52.3k");
    assert_eq!(fit::tokens(7_900.0), "7.9k");
    assert_eq!(fit::count(1_840_000.0), "1.84M");
    assert_eq!(fit::time(3_200.0), "3.2s");
    assert_eq!(fit::time(65_000.0), "1m 05s");
    assert_eq!(fit::thought_time(8_100.0), "9s");
    assert_eq!(fit::clip("abcdef", 4), "abc…");
    assert_eq!(fit::cut_text("ab   cdef", 6), "ab…");
}
