//! hook 大输出落盘（docs/specs/rust-cli/hooks.md §4.5）：阈值边界、预览文字、各通道。

use std::sync::Arc;

use rust_cli_kernel_api::hooks::{HookEventName, HookOutcome, HookRunner};
use rust_cli_kernel_api::ids::{ArtifactId, SessionId};
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::store::{ArtifactPurpose, ArtifactRef, ArtifactStore};
use rust_cli_kernel_api::testing::{MemoryArtifacts, MemoryStore};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::test_support::{ScriptedEnv, command_settings, engine, event, script};

/// 只有一个 command hook `big.sh` 的事件，脚本给出 `stdout` / `stderr` / 退出码。
async fn run(
    name: HookEventName,
    matcher: &str,
    payload: serde_json::Value,
    stdout: &str,
    stderr: &str,
    exit: i32,
) -> HookOutcome {
    let env = Arc::new(ScriptedEnv::with(&[(
        "big.sh",
        script(stdout, stderr, exit),
    )]));
    let engine = engine(env, &command_settings(name.as_str(), matcher, "big.sh"));
    engine
        .run(event(name, payload), CancellationToken::new())
        .await
}

fn post_tool_use() -> serde_json::Value {
    json!({"tool_name": "Bash", "tool_input": {"command": "x"}, "tool_response": "", "tool_use_id": "toolu_1"})
}

fn context_json(event: &str, text: &str) -> String {
    json!({"hookSpecificOutput": {"hookEventName": event, "additionalContext": text}}).to_string()
}

fn texts(outcome: &HookOutcome) -> Vec<String> {
    outcome
        .messages
        .iter()
        .map(|message| match message {
            Message::Attachment(attachment) => attachment.text.clone(),
            other => panic!("unexpected {other:?}"),
        })
        .collect()
}

/// 落盘提示的整段文字（位置是第 `n` 个落盘的 artifact）。
fn persisted(n: usize, size: &str, preview: &str, more: bool) -> String {
    format!(
        "<persisted-output>\nOutput too large ({size}). Full output saved to: zcode-artifact://sess_1/tool-result-{n}\n\nPreview (first 2KB):\n{preview}{}</persisted-output>",
        if more { "\n...\n" } else { "\n" }
    )
}

#[tokio::test]
async fn additional_context_at_the_limit_stays_inline() {
    let exact = "a".repeat(10_000);
    let outcome = run(
        HookEventName::PostToolUse,
        "Bash",
        post_tool_use(),
        &context_json("PostToolUse", &exact),
        "",
        0,
    )
    .await;
    assert_eq!(outcome.additional_context, std::slice::from_ref(&exact));
    assert_eq!(
        texts(&outcome),
        [format!(
            "<system-reminder>\nPostToolUse:Bash hook additional context: {exact}\n</system-reminder>"
        )]
    );
}

#[tokio::test]
async fn additional_context_past_the_limit_is_persisted_with_a_preview() {
    // 10001 个字符、没有换行：预览截在 2000，大小按 UTF-16 码元（9.8KB）。
    let over = "b".repeat(10_001);
    let outcome = run(
        HookEventName::PostToolUse,
        "Bash",
        post_tool_use(),
        &context_json("PostToolUse", &over),
        "",
        0,
    )
    .await;
    let expected = persisted(1, "9.8KB", &"b".repeat(2_000), true);
    assert_eq!(outcome.additional_context, std::slice::from_ref(&expected));
    assert_eq!(
        texts(&outcome),
        [format!(
            "<system-reminder>\nPostToolUse:Bash hook additional context: {expected}\n</system-reminder>"
        )]
    );
}

#[tokio::test]
async fn the_limit_counts_utf16_units() {
    // 5000 个 emoji 是 10000 个码元（20000 字节）：不落盘；5001 个落盘，预览是前 1000 个。
    let exact = "😀".repeat(5_000);
    let inline = run(
        HookEventName::PostToolUse,
        "Bash",
        post_tool_use(),
        &context_json("PostToolUse", &exact),
        "",
        0,
    )
    .await;
    assert_eq!(inline.additional_context, [exact]);
    let over = run(
        HookEventName::PostToolUse,
        "Bash",
        post_tool_use(),
        &context_json("PostToolUse", &"😀".repeat(5_001)),
        "",
        0,
    )
    .await;
    assert_eq!(
        over.additional_context,
        [persisted(1, "9.8KB", &"😀".repeat(1_000), true)]
    );
}

#[tokio::test]
async fn session_start_plain_stdout_is_persisted_with_a_line_preview() {
    // 大输出的 SessionStart：1200 行，去空白后 20399 个字符；预览截在第 117 行末的换行处。
    let stdout: String = (1..=1200).map(|i| format!("start line {i:05}\n")).collect();
    let outcome = run(
        HookEventName::SessionStart,
        "",
        json!({"source": "startup"}),
        &stdout,
        "",
        0,
    )
    .await;
    let preview: Vec<String> = (1..=117).map(|i| format!("start line {i:05}")).collect();
    assert_eq!(
        texts(&outcome),
        [format!(
            "<system-reminder>\nSessionStart:startup hook success: {}\n</system-reminder>",
            persisted(1, "19.9KB", &preview.join("\n"), true)
        )]
    );
}

#[tokio::test]
async fn user_prompt_submit_plain_stdout_is_persisted() {
    let outcome = run(
        HookEventName::UserPromptSubmit,
        "",
        json!({"prompt": "hi"}),
        &"c".repeat(10_001),
        "",
        0,
    )
    .await;
    assert_eq!(
        texts(&outcome),
        [format!(
            "<system-reminder>\nUserPromptSubmit hook success: {}\n</system-reminder>",
            persisted(1, "9.8KB", &"c".repeat(2_000), true)
        )]
    );
}

#[tokio::test]
async fn system_message_and_initial_user_message_are_persisted() {
    let stdout = json!({
        "systemMessage": "s".repeat(10_001),
        "hookSpecificOutput": {"hookEventName": "SessionStart", "initialUserMessage": "u".repeat(10_001)}
    })
    .to_string();
    let outcome = run(
        HookEventName::SessionStart,
        "",
        json!({"source": "startup"}),
        &stdout,
        "",
        0,
    )
    .await;
    // 先处理 systemMessage，再处理 initialUserMessage。
    assert_eq!(
        outcome.notices,
        [persisted(1, "9.8KB", &"s".repeat(2_000), true)]
    );
    assert_eq!(
        outcome.initial_user_message,
        Some(persisted(2, "9.8KB", &"u".repeat(2_000), true))
    );
}

#[tokio::test]
async fn blocking_errors_are_not_persisted() {
    // 退出码 2 的 stderr 是阻塞原因：不落盘，原样交给模型。
    let stderr = "e".repeat(10_001);
    let outcome = run(
        HookEventName::PostToolUse,
        "Bash",
        post_tool_use(),
        "",
        &stderr,
        2,
    )
    .await;
    assert_eq!(outcome.blocking_error, Some(format!("[big.sh]: {stderr}")));
}

/// 带指定存储的引擎（看落盘的内容与次数）。
fn engine_with(
    artifacts: Arc<dyn ArtifactStore>,
    name: HookEventName,
    matcher: &str,
    stdout: &str,
) -> crate::HookEngine {
    let env = Arc::new(ScriptedEnv::with(&[("big.sh", script(stdout, "", 0))]));
    crate::HookEngine::with_config(
        crate::HookPorts {
            artifacts,
            ..crate::test_support::ports(env)
        },
        crate::test_support::config(&command_settings(name.as_str(), matcher, "big.sh")),
    )
}

#[tokio::test]
async fn the_whole_text_is_stored_and_unused_stdout_is_not() {
    let artifacts = Arc::new(MemoryArtifacts::default());
    let stdout = format!("  {}\n", "d".repeat(10_001));
    let start = engine_with(artifacts.clone(), HookEventName::SessionStart, "", &stdout);
    start
        .run(
            event(HookEventName::SessionStart, json!({"source": "startup"})),
            CancellationToken::new(),
        )
        .await;
    // 存的是去空白后的全文。
    let stored = ArtifactRef {
        session: SessionId::new("sess_1"),
        id: ArtifactId::new("tool-result-1"),
        purpose: ArtifactPurpose::ToolOutput,
        media_type: "text/plain".to_owned(),
        size: 10_001,
    };
    assert_eq!(
        artifacts.get(&stored).await.unwrap(),
        "d".repeat(10_001).into_bytes()
    );
    // PreToolUse 的纯文本 stdout 不进模型、本工程也不用：不落盘（D13）。
    let pre = engine_with(
        artifacts.clone(),
        HookEventName::PreToolUse,
        "Bash",
        &stdout,
    );
    pre.run(
        event(
            HookEventName::PreToolUse,
            json!({"tool_name": "Bash", "tool_input": {"command": "x"}, "tool_use_id": "toolu_1"}),
        ),
        CancellationToken::new(),
    )
    .await;
    assert_eq!(artifacts.count(), 1);
}

#[tokio::test]
async fn a_failed_persist_keeps_the_head_and_says_why() {
    // `MemoryStore` 的 artifact 不可用：保留前 10000 个码元。
    let engine = engine_with(
        Arc::new(MemoryStore::default()),
        HookEventName::PostToolUse,
        "Bash",
        &context_json("PostToolUse", &"f".repeat(10_001)),
    );
    let outcome = engine
        .run(
            event(HookEventName::PostToolUse, post_tool_use()),
            CancellationToken::new(),
        )
        .await;
    assert_eq!(
        outcome.additional_context,
        [format!(
            "{}\n\n[Hook additionalContext truncated at 10000 chars — persist-to-disk failed: not implemented: memory artifacts]",
            "f".repeat(10_000)
        )]
    );
}
