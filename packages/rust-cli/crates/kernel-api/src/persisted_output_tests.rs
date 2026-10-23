//! `<persisted-output>` 的预览与文字，以及唯一落盘入口 `persist`。

use std::sync::Mutex;

use async_trait::async_trait;

use crate::ids::{ArtifactId, SessionId};
use crate::store::{ArtifactPurpose, ArtifactRef, ArtifactStore, StoreError};

use super::*;

/// 记下每次 put 的内存存储（kernel-api 自己的测试拿不到 testing feature 的替身）。
#[derive(Default)]
struct Recorded(Mutex<Vec<(ArtifactPurpose, String, Vec<u8>)>>);

#[async_trait]
impl ArtifactStore for Recorded {
    async fn put(
        &self,
        session: &SessionId,
        purpose: ArtifactPurpose,
        media_type: String,
        bytes: Vec<u8>,
    ) -> Result<ArtifactRef, StoreError> {
        let mut puts = self.0.lock().unwrap();
        puts.push((purpose, media_type.clone(), bytes.clone()));
        Ok(ArtifactRef {
            session: session.clone(),
            id: ArtifactId::new(format!("tool-result-{}", puts.len())),
            purpose,
            media_type,
            size: bytes.len() as u64,
        })
    }

    async fn get(&self, _artifact: &ArtifactRef) -> Result<Vec<u8>, StoreError> {
        Ok(Vec::new())
    }
}

#[test]
fn preview_keeps_short_text_and_cuts_long_text() {
    assert_eq!(preview("short"), ("short", false));
    let exact = "x".repeat(PREVIEW_UNITS);
    assert_eq!(preview(&exact), (exact.as_str(), false));
    // 没有换行：截在 2000。
    let flat = "y".repeat(PREVIEW_UNITS + 1);
    assert_eq!(preview(&flat), (&flat[..PREVIEW_UNITS], true));
    // 换行在一半之后：截在最后一个换行处（不含换行）。
    let lines: String = (1..=200).map(|i| format!("line {i:05}\n")).collect();
    let (shown, more) = preview(&lines);
    assert!(more);
    assert_eq!(shown.len(), 181 * 11 - 1);
    assert!(shown.ends_with("line 00181"));
    // 换行只在一半之前：直接截在 2000。
    let early = format!("{}\n{}", "a".repeat(10), "b".repeat(3000));
    assert_eq!(preview(&early).0.len(), PREVIEW_UNITS);
    // 按 UTF-16 码元计，不拆开代理对。
    let emoji = "😀".repeat(1_001);
    assert_eq!(preview(&emoji), ("😀".repeat(1_000).as_str(), true));
}

#[test]
fn message_is_the_persisted_output_text() {
    assert_eq!(
        message("zcode-artifact://s/a", 10_001, "head", true),
        "<persisted-output>\nOutput too large (9.8KB). Full output saved to: zcode-artifact://s/a\n\nPreview (first 2KB):\nhead\n...\n</persisted-output>"
    );
    assert_eq!(
        message("/tmp/out", 1_500, "all", false),
        "<persisted-output>\nOutput too large (1.5KB). Full output saved to: /tmp/out\n\nPreview (first 2KB):\nall\n</persisted-output>"
    );
}

#[test]
fn format_bytes_uses_one_decimal_units() {
    assert_eq!(format_bytes(512), "512 bytes");
    assert_eq!(format_bytes(2000), "2KB");
    assert_eq!(format_bytes(99_738), "97.4KB");
    assert_eq!(format_bytes(1_073_741_824), "1GB");
}

#[tokio::test]
async fn persist_stores_the_whole_text_and_returns_the_preview() {
    let artifacts = Recorded::default();
    let session = SessionId::new("sess_1");
    let body = "z".repeat(10_001);
    let text = persist(&artifacts, &session, &body, "text/plain")
        .await
        .unwrap();
    assert_eq!(
        text,
        message(
            "zcode-artifact://sess_1/tool-result-1",
            10_001,
            &"z".repeat(PREVIEW_UNITS),
            true
        )
    );
    assert_eq!(
        *artifacts.0.lock().unwrap(),
        [(
            ArtifactPurpose::ToolOutput,
            "text/plain".to_owned(),
            body.into_bytes()
        )]
    );
}
