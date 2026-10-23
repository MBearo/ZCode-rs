use async_trait::async_trait;
use rust_cli_kernel_api::ids::ArtifactId;
use rust_cli_kernel_api::store::{ArtifactPurpose, ArtifactRef, StoreError};

use super::*;

struct Store;

#[async_trait]
impl ArtifactStore for Store {
    async fn put(
        &self,
        session: &SessionId,
        purpose: ArtifactPurpose,
        media_type: String,
        bytes: Vec<u8>,
    ) -> Result<ArtifactRef, StoreError> {
        Ok(ArtifactRef {
            session: session.clone(),
            id: ArtifactId::new("art_1"),
            purpose,
            media_type,
            size: bytes.len() as u64,
        })
    }

    async fn get(&self, _artifact: &ArtifactRef) -> Result<Vec<u8>, StoreError> {
        Ok(Vec::new())
    }
}

fn output(text: &str) -> ToolOutput {
    ToolOutput {
        content: vec![ToolResultContent::Text {
            text: text.to_owned(),
        }],
        is_error: false,
        display: Value::Null,
    }
}

fn first(output: &ToolOutput) -> &str {
    match &output.content[0] {
        ToolResultContent::Text { text } => text,
        ToolResultContent::Image { .. }
        | ToolResultContent::Document { .. }
        | ToolResultContent::ToolReference { .. } => "",
    }
}

#[tokio::test]
async fn results_over_the_threshold_are_persisted_with_a_preview() {
    let session = SessionId::new("sess_1");
    let line = "x".repeat(99);
    let long = vec![line.as_str(); 300].join("\n");
    let persisted = apply(output(&long), "Grep", Some(20_000), &session, &Store).await;
    let text = first(&persisted);
    assert!(text.starts_with("<persisted-output>\nOutput too large (29.3KB). Full output saved to: zcode-artifact://sess_1/art_1\n\nPreview (first 2KB):\n"));
    assert!(text.ends_with("\n...\n</persisted-output>"));
    let short = apply(output("small"), "Grep", Some(20_000), &session, &Store).await;
    assert_eq!(first(&short), "small");
    let unlimited = apply(output(&long), "Read", None, &session, &Store).await;
    assert_eq!(first(&unlimited), long);
    let empty = apply(output("  "), "Glob", Some(100_000), &session, &Store).await;
    assert_eq!(first(&empty), "(Glob completed with no output)");
}
