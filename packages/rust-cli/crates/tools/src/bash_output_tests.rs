//! Bash 结果文本（期望值逐字固定）。

use rust_cli_kernel_api::ids::{OperationId, ShellTaskId};

use super::*;

#[test]
fn task_ids_are_stable_and_well_shaped() {
    let id = task_id(&OperationId::new("op_1"));
    assert_eq!(id, task_id(&OperationId::new("op_1")));
    assert_ne!(id, task_id(&OperationId::new("op_2")));
    assert_eq!(id.as_str().len(), 9);
    assert!(id.as_str().starts_with('b'));
    assert!(
        id.as_str()[1..]
            .chars()
            .all(|c| c.is_ascii_digit() || c.is_ascii_lowercase())
    );
}

#[test]
fn durations_and_exit_code_semantics() {
    assert!(!is_error("grep", 1));
    assert!(is_error("grep", 2));
    assert!(!is_error("diff", 1));
    assert!(is_error("npm", 1));
    assert!(!is_error("npm", 0));
}

/// 超时文字里的时长不带选项地格式化（与 kernel-api `metering::format_duration` 同一个，
/// 修复线 F58）：整分钟也写秒，`2m 0s`；有天时写到分钟。以前这里另有一份只写非零单位的格式（`2m`、`1h 5s`）。
#[test]
fn timeouts_spell_the_duration_in_full() {
    assert_eq!(timed_out(120_000, ""), "Command timed out after 2m 0s");
    assert_eq!(timed_out(3_605_000, ""), "Command timed out after 1h 0m 5s");
    assert_eq!(timed_out(90_500, ""), "Command timed out after 1m 31s");
    assert_eq!(
        timed_out(86_400_000, ""),
        "Command timed out after 1d 0h 0m"
    );
    assert_eq!(timed_out(59_999, ""), "Command timed out after 59s");
}

#[test]
fn failures_and_timeouts_put_the_exit_code_first() {
    assert_eq!(failure(3, "out\nerr\n"), "Exit code 3\nout\nerr\n");
    assert_eq!(failure(1, ""), "Exit code 1");
    assert_eq!(timed_out(1_000, ""), "Command timed out after 1s");
    assert_eq!(
        timed_out(1_000, "early\n"),
        "Command timed out after 1s\nearly\n"
    );
}

#[test]
fn success_strips_blank_lines_and_reports_cwd_resets() {
    assert_eq!(strip_output("\n\n  \nfirst\n\nlast  \n\n"), "first\n\nlast");
    assert_eq!(
        success("/\n", 2, "/tmp/b.output", Some("/w")),
        "/\nShell cwd was reset to /w"
    );
    assert_eq!(success("", 0, "/tmp/b.output", None), "");
}

#[test]
fn large_outputs_point_at_the_output_file() {
    let text: String = (1..=20_000).map(|n| format!("{n}\n")).collect();
    let head = &text[..INLINE_LIMIT];
    let shown = success(head, text.len() as u64, "/tmp/b1.output", None);
    assert!(shown.starts_with(
        "<persisted-output>\nOutput too large (106.3KB). Full output saved to: /tmp/b1.output\n\nPreview (first 2KB):\n1\n2\n"
    ));
    assert!(shown.ends_with("526\n527\n...\n</persisted-output>"));
}

#[test]
fn background_texts_are_verbatim() {
    let task = ShellTaskId::new("bxwig2fjb");
    assert_eq!(
        backgrounded(&task, "/t/bxwig2fjb.output"),
        "Command running in background with ID: bxwig2fjb. Output is being written to: /t/bxwig2fjb.output. You will be notified when it completes. To check interim output, use Read on that file path."
    );
}
