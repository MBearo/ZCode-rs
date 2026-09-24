//! Node `bash-model-content.ts`, `bash-semantics.ts` and TaskOutput/TaskStop texts.
use super::*;

fn done(status: &str, exit: Option<i64>, output: &str) -> Value {
    let mut data = json!({"status": status, "stdout": output, "persistedOutputSize": output.len(),
        "stderr": "", "interrupted": false, "persistedOutputPath": "/tmp/o.output"});
    if let Some(code) = exit {
        data["exitCode"] = code.into();
    }
    data
}

#[test]
fn success_and_provider_errors() {
    let (text, error) = bash_content("echo hi", &done("completed", Some(0), "\n  \nhello\n  \n"));
    assert_eq!((text.as_str(), error), ("hello", false));
    let (text, error) = bash_content("make", &done("failed", Some(2), "boom\n"));
    assert_eq!((text.as_str(), error), ("Exit code 2\nboom", true));
    assert_eq!(bash_content("true", &done("completed", Some(0), "")).0, "");
}

#[test]
fn exit_one_of_search_and_compare_commands_is_not_an_error() {
    for command in [
        "grep foo x",
        "cat x | rg foo",
        "git grep foo",
        "git -C repo diff --quiet",
        "find . -name x",
        "diff a b",
        "[ -f x ]",
    ] {
        let (text, error) = bash_content(command, &done("failed", Some(1), "out"));
        assert_eq!((text.as_str(), error), ("out", false), "{command}");
    }
    // Node 同样把未闭合 `$(` 的 grep 视为无匹配（已用 interpretBashReturnCode 核对）。
    assert!(!bash_content("grep foo $(x", &done("failed", Some(1), "")).1);
    for command in ["grep foo x && false", "git log"] {
        assert!(
            bash_content(command, &done("failed", Some(1), "")).1,
            "{command}"
        );
    }
    assert!(bash_content("grep foo x", &done("failed", Some(2), "")).1);
}

#[test]
fn interruptions_backgrounds_and_large_output() {
    let mut timed_out = done("timed_out", None, "partial");
    timed_out["interrupted"] = true.into();
    timed_out["stderr"] = "Command timed out after 2m".into();
    assert_eq!(
        bash_content("sleep 1000", &timed_out),
        (
            "partial\nCommand timed out after 2m\n<error>Command was aborted before completion</error>".into(),
            true
        )
    );
    let background = json!({"status": "backgrounded", "backgroundTaskId": "t1",
        "persistedOutputPath": "/tmp/t1.output", "stdout": "", "stderr": "", "interrupted": false});
    assert_eq!(
        bash_content("npm run dev", &background).0,
        "Command running in background with ID: t1. Output is being written to: /tmp/t1.output. You will be notified when it completes. To check interim output, use Read on that file path."
    );
    let mut large = done("completed", Some(0), &"x".repeat(100));
    large["persistedOutputSize"] = 40_000.into();
    let (text, _) = bash_content("yes", &large);
    assert!(text.starts_with("<persisted-output>\nOutput too large (39.1KB). Full output saved to: /tmp/o.output\n\nPreview (first 2KB):\n"));
}

#[test]
fn durations_and_task_texts() {
    assert_eq!(
        [500, 30_000, 90_000, 120_000, 125_000, 7_200_000].map(duration),
        ["500ms", "30s", "1.5m", "2m", "2.1m", "2h"]
    );
    let task = json!({"task_id": "t1", "task_type": "local_bash", "status": "completed",
        "exitCode": 0, "output": "line\n\n", "outputFile": "/tmp/t1.output"});
    assert_eq!(
        task_output_content("success", &task),
        "<retrieval_status>success</retrieval_status>\n\n<task_id>t1</task_id>\n\n<task_type>local_bash</task_type>\n\n<status>completed</status>\n\n<exit_code>0</exit_code>\n\n<output>\nline\n</output>"
    );
    let long = json!({"task_id": "t", "task_type": "local_bash", "status": "running",
        "output": "y".repeat(40_000), "outputFile": "/f"});
    let text = task_output_content("not_ready", &long);
    assert!(text.contains("<output>\n[Truncated. Full output: /f]\n\nyyy"));
    assert_eq!(
        task_stop_content("t1", "npm run dev").0,
        r#"{"message":"Successfully stopped task: t1 (npm run dev)","task_id":"t1","task_type":"local_bash","command":"npm run dev"}"#
    );
}
