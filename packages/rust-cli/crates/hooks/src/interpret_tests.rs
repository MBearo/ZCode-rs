//! 退出码语义与“外部”路径（spec §4.1、§4.3）。

use super::*;
use crate::result::Blocking;

fn command(stdout: &str, stderr: &str, exit: i32) -> Executed {
    Executed::Raw(Raw::Command {
        stdout: stdout.to_owned(),
        stderr: stderr.to_owned(),
        exit,
    })
}

#[test]
fn exit_codes() {
    let ok = repl(command("  hello \n", "", 0), "c", "UserPromptSubmit");
    assert_eq!(ok.outcome, Outcome::Success);
    assert_eq!(ok.success_content.as_deref(), Some("hello"));
    let blocked = repl(command("", "  bad thing\n", 2), "guard.sh", "PreToolUse");
    assert_eq!(
        blocked.blocking,
        Some(Blocking {
            error: "[guard.sh]:   bad thing\n".to_owned(),
            command: "guard.sh".to_owned()
        })
    );
    let silent = repl(command("", "", 2), "g", "PreToolUse");
    assert_eq!(silent.blocking.unwrap().error, "[g]: No stderr output");
    let other = repl(command("out", " oops \n", 1), "c", "PreToolUse");
    assert_eq!(other.outcome, Outcome::NonBlockingError);
    assert_eq!(
        other.error.as_deref(),
        Some("Failed with non-blocking status code: oops")
    );
}

#[test]
fn json_and_exit_two() {
    // JSON 合法且退出码 2、JSON 没给阻塞：用 stderr 阻塞。
    let both = repl(command("{\"systemMessage\": \"m\"}", "err", 2), "c", "Stop");
    assert_eq!(both.blocking.unwrap().error, "[c]: err");
    assert_eq!(both.system_message.as_deref(), Some("m"));
    // JSON 自己给了阻塞：保留 JSON 的原因。
    let own = repl(
        command("{\"decision\": \"block\", \"reason\": \"r\"}", "err", 2),
        "c",
        "Stop",
    );
    assert_eq!(own.blocking.unwrap().error, "r");
    // 校验失败：退出码 2 时仍按 stderr 阻塞，否则非阻塞错误（附 stderr）。
    let invalid_two = repl(command("{\"continue\": 1}", "e", 2), "c", "Stop");
    assert_eq!(invalid_two.blocking.unwrap().error, "[c]: e");
    let invalid = repl(command("{\"continue\": 1}", "e", 3), "c", "Stop");
    let error = invalid.error.unwrap();
    assert!(error.starts_with("JSON validation failed: Hook JSON output validation failed"));
    assert!(error.ends_with("\n\nHook exited 3 with stderr:\ne"));
    let wrong = repl(
        command(
            "{\"hookSpecificOutput\": {\"hookEventName\": \"Stop\"}}",
            "",
            0,
        ),
        "c",
        "PreToolUse",
    );
    assert!(
        wrong
            .error
            .unwrap()
            .starts_with("Failed to run: Hook returned incorrect event name")
    );
}

#[test]
fn async_cancel_and_failures() {
    assert_eq!(
        repl(command("{\"async\": true}", "", 0), "c", "Stop").outcome,
        Outcome::Success
    );
    assert!(
        repl(command("{\"async\": true}", "", 1), "c", "Stop")
            .error
            .unwrap()
            .starts_with("Announced async, then failed with status code 1")
    );
    assert_eq!(
        repl(Executed::Raw(Raw::Cancelled), "c", "Stop").outcome,
        Outcome::Cancelled
    );
    let failed = repl(
        Executed::Raw(Raw::Failed("Failed to run: x".to_owned())),
        "c",
        "Stop",
    );
    assert_eq!(failed.error.as_deref(), Some("Failed to run: x"));
}

#[test]
fn http_results() {
    let http = |status, body: &str| {
        repl(
            Executed::Raw(Raw::Http {
                status,
                body: body.to_owned(),
            }),
            "https://hooks.example.com/h",
            "PreToolUse",
        )
    };
    assert_eq!(
        http(500, "").error.as_deref(),
        Some("HTTP 500 from https://hooks.example.com/h")
    );
    assert_eq!(http(200, "").outcome, Outcome::Success);
    assert!(
        http(200, "nope")
            .error
            .unwrap()
            .starts_with("JSON validation failed: HTTP hook must return JSON")
    );
    let deny = http(200, "{\"decision\": \"block\"}");
    assert_eq!(
        deny.blocking.unwrap().command,
        "https://hooks.example.com/h"
    );
}

#[test]
fn outside_summaries() {
    let summary = outside(command("out", "err", 0), "c");
    assert!(summary.succeeded && !summary.blocked);
    assert_eq!(summary.output, "out");
    let failed = outside(command("out", "err", 1), "c");
    assert_eq!((failed.succeeded, failed.output.as_str()), (false, "err"));
    let blocked = outside(command("out", "err", 2), "c");
    assert!(blocked.blocked && !blocked.succeeded);
    let json_block = outside(
        command("{\"decision\": \"block\", \"reason\": \"r\"}", "e", 0),
        "c",
    );
    assert_eq!(
        (json_block.blocked, json_block.output.as_str()),
        (true, "r")
    );
    let cancelled = outside(Executed::Raw(Raw::Cancelled), "c");
    assert_eq!(cancelled.output, "Hook cancelled");
    assert!(cancelled.cancelled);
    let invalid = outside(command("{\"continue\": 1}", "", 0), "c");
    assert!(
        !invalid.succeeded
            && invalid
                .output
                .starts_with("Hook JSON output validation failed")
    );
}
