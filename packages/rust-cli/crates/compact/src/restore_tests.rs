use rust_cli_kernel_api::message::Message;

use super::{candidates, render_file};
use crate::test_support::tool_round;

/// 候选来自 readFileState 的复制品（最近的在前）：去掉保留尾段里 Read 过的，最多 5 个。
#[test]
fn candidates_skip_files_read_in_the_kept_tail_and_cap_at_five() {
    let recent: Vec<String> = (0..7).rev().map(|index| format!("/w/{index}.rs")).collect();
    let kept: Vec<Message> = tool_round(6, "Read", "/w/6.rs").to_vec();
    let kept_refs: Vec<&Message> = kept.iter().collect();
    assert_eq!(
        candidates(&recent, &kept_refs),
        ["/w/5.rs", "/w/4.rs", "/w/3.rs", "/w/2.rs", "/w/1.rs"]
    );
}

/// 调用说明与结果说明之间只隔一个换行（折进 system 消息的文件附件是
/// `Called the Read tool …\nResult of calling the Read tool:\n…`）。
#[test]
fn the_file_attachment_joins_the_call_and_the_result_with_one_newline() {
    let text = render_file("/Users/dev/a.txt", "1\talpha\n");
    assert_eq!(
        text,
        "<system-reminder>\nCalled the Read tool with the following input: {\"file_path\":\"/Users/dev/a.txt\"}\nResult of calling the Read tool:\n1\talpha\n\n</system-reminder>"
    );
}
