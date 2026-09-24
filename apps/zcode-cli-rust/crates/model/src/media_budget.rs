//! Node `projectMessagesForMediaBudget` (spec rust-m11-node-storage §5.3): a
//! request's images, videos and PDFs stay within 40 MiB of data URL bytes.
//! The latest real user message's media are kept first, then history from
//! the newest message and its last block; what no longer fits is left out.
use crate::contract::ModelFailure;
use serde_json::Value;
use std::collections::HashSet;

/// Node `DEFAULT_MODEL_REQUEST_MEDIA_BUDGET_BYTES`.
pub(super) const BUDGET: u64 = 40 * 1024 * 1024;

/// Node `mediaOmittedTextBlock` suffix.
pub(super) const OMITTED: &str = "[Media omitted from provider request to keep the request body under the configured media budget.]";

/// One media block the request would send.
#[derive(Clone, Copy, Debug)]
pub(super) struct MediaRef {
    pub message: usize,
    pub block: usize,
    /// Node `mediaRequestBytes`: the UTF-8 length of its data URL.
    pub bytes: u64,
}

/// Node `isCanonicalRealUser` over request messages: a user message that is
/// not a tool result, has no runtime source and is not a reminder.
pub(super) fn real_user(message: &Value) -> bool {
    if message["role"] != "user"
        || message.get("_zcode_source").is_some()
        || message.get("tool_call_id").is_some()
    {
        return false;
    }
    let first = match &message["content"] {
        Value::String(text) => Some(text.as_str()),
        Value::Array(parts) => parts.first().and_then(|p| p["text"].as_str()),
        _ => None,
    };
    first.is_none_or(|text| {
        let text = text.trim_start_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
        !text.starts_with("<system-reminder>") && !text.starts_with("<task-notification>")
    })
}

/// The `(message, block)` positions left out of the request, or Node's
/// failure when the latest real user message's media alone exceed `budget`.
pub(super) fn omitted(
    media: &[MediaRef],
    latest_user: Option<usize>,
    budget: u64,
) -> Result<HashSet<(usize, usize)>, ModelFailure> {
    let total: u64 = media.iter().map(|m| m.bytes).sum();
    if total <= budget {
        return Ok(HashSet::new());
    }
    let protected = |m: &&MediaRef| Some(m.message) == latest_user;
    let kept: u64 = media.iter().filter(protected).map(|m| m.bytes).sum();
    if kept > budget {
        return Err(ModelFailure::new("media_budget", false));
    }
    let mut remaining = budget - kept;
    let mut history: Vec<&MediaRef> = media.iter().filter(|m| !protected(m)).collect();
    history.sort_by(|a, b| b.message.cmp(&a.message).then(b.block.cmp(&a.block)));
    let mut omitted = HashSet::new();
    for media in history {
        // Node：放不下的跳过，继续尝试更早的较小媒体。
        if media.bytes > remaining {
            omitted.insert((media.message, media.block));
        } else {
            remaining -= media.bytes;
        }
    }
    Ok(omitted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn at(message: usize, block: usize, bytes: u64) -> MediaRef {
        MediaRef {
            message,
            block,
            bytes,
        }
    }

    #[test]
    fn keeps_everything_within_the_budget() {
        let media = [at(1, 0, 40), at(3, 1, 60)];
        assert!(omitted(&media, Some(3), 100).unwrap().is_empty());
    }

    #[test]
    fn protects_the_latest_user_media_then_fills_history_newest_first() {
        // 超预算：最新真实用户消息（3）的 40 受保护，剩余 60 从新到旧装入 (2,1)、(2,0)。
        let media = [
            at(0, 0, 20),
            at(1, 0, 50),
            at(2, 0, 30),
            at(2, 1, 30),
            at(3, 0, 40),
        ];
        let left: HashSet<_> = omitted(&media, Some(3), 100).unwrap();
        assert_eq!(left, HashSet::from([(0, 0), (1, 0)]));
        // 较新的大媒体放不下时跳过，更早的小媒体仍可装入。
        let media = [at(0, 0, 10), at(1, 0, 70), at(2, 0, 40)];
        let left: HashSet<_> = omitted(&media, Some(2), 100).unwrap();
        assert_eq!(left, HashSet::from([(1, 0)]));
    }

    #[test]
    fn fails_when_the_current_attachments_alone_exceed_it() {
        let failure = omitted(&[at(0, 0, 10), at(3, 0, 101)], Some(3), 100).unwrap_err();
        assert_eq!(failure.code, "MEDIA_BUDGET_CURRENT_ATTACHMENT_TOO_LARGE");
        assert!(!failure.retryable);
        // 没有真实用户消息时没有受保护媒体。
        assert_eq!(omitted(&[at(0, 0, 101)], None, 100).unwrap().len(), 1);
    }

    #[test]
    fn real_user_excludes_runtime_messages() {
        assert!(real_user(&json!({"role": "user", "content": "hi"})));
        assert!(real_user(
            &json!({"role": "user", "content": [{"type": "image"}, {"type": "text", "text": "x"}]})
        ));
        assert!(!real_user(
            &json!({"role": "user", "content": "  <system-reminder>\nx"})
        ));
        assert!(!real_user(
            &json!({"role": "user", "content": "<task-notification>x"})
        ));
        assert!(!real_user(
            &json!({"role": "user", "content": "x", "_zcode_source": "todo_reminder"})
        ));
        assert!(!real_user(&json!({"role": "tool", "content": "x"})));
    }
}
