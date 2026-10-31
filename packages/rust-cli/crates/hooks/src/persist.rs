//! hook 大输出落盘（docs/specs/rust-cli/hooks.md §4.5）。
//!
//! 修复线 F15（原 D13）：以前 hook 输出不论多长都原样进模型上下文；超过 10000 个 UTF-16 码元时应把全文
//! 落盘、只给 `<persisted-output>` 预览。改为在每个 hook 的结果折叠之前，按通道逐个经 kernel-api 的
//! `persisted_output::persist`（与工具结果同一入口，ArtifactStore）处理；本 crate 不做 IO。

use rust_cli_base::js::string::utf16_prefix;
use rust_cli_kernel_api::hooks::HookEventName;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::persisted_output;
use rust_cli_kernel_api::store::ArtifactStore;

use crate::result::HookResult;

/// 落盘阈值，按 UTF-16 码元；恰好等于时不落盘。
pub(crate) const THRESHOLD: usize = 10_000;

/// `mae(text, …, source)`：超过阈值时落盘并返回预览；落盘失败时保留前 10000 个码元并写明原因。
async fn output(
    artifacts: &dyn ArtifactStore,
    session: &SessionId,
    text: String,
    source: &str,
) -> String {
    if text.encode_utf16().count() <= THRESHOLD {
        return text;
    }
    match persisted_output::persist(
        artifacts,
        session,
        &text,
        rust_cli_base::proto::mime::TEXT_PLAIN,
    )
    .await
    {
        Ok(preview) => preview,
        Err(error) => {
            tracing::warn!(source, %error, "hook output could not be persisted");
            format!(
                "{}\n\n[Hook {source} truncated at {THRESHOLD} chars \u{2014} persist-to-disk failed: {error}]",
                utf16_prefix(&text, THRESHOLD)
            )
        }
    }
}

async fn field(
    artifacts: &dyn ArtifactStore,
    session: &SessionId,
    value: &mut Option<String>,
    source: &str,
) {
    if let Some(text) = value.take() {
        *value = Some(output(artifacts, session, text, source).await);
    }
}

/// 按完成顺序逐个处理（依次 await）。通道的先后：纯文本 stdout 在执行时，
/// 之后是 `systemMessage`、`additionalContext`、`initialUserMessage`。`success_content` 只有
/// SessionStart / UserPromptSubmit 用到，其他事件不落盘（D13）。
pub(crate) async fn results(
    artifacts: &dyn ArtifactStore,
    session: &SessionId,
    event: HookEventName,
    results: &mut [HookResult],
) {
    let success_used = matches!(
        event,
        HookEventName::SessionStart | HookEventName::UserPromptSubmit
    );
    for result in results {
        if success_used {
            field(artifacts, session, &mut result.success_content, "stdout").await;
        }
        field(
            artifacts,
            session,
            &mut result.system_message,
            "systemMessage",
        )
        .await;
        field(
            artifacts,
            session,
            &mut result.additional_context,
            "additionalContext",
        )
        .await;
        field(
            artifacts,
            session,
            &mut result.initial_user_message,
            "initialUserMessage",
        )
        .await;
    }
}

#[cfg(test)]
#[path = "persist_tests.rs"]
mod tests;
