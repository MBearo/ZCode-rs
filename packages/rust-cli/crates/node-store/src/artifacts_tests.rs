// segment 的用例复制自 apps/zcode-cli-rust/crates/state/src/node/artifacts.rs（feat/rs 79b6691c55）。
use std::path::Path;

use rust_cli_kernel_api::ids::{ArtifactId, SessionId};
use rust_cli_paths::{StoragePaths, StorageSettings};

use super::*;

fn artifact(session: &str, purpose: ArtifactPurpose, media_type: &str) -> ArtifactRef {
    ArtifactRef {
        session: SessionId::new(session),
        id: ArtifactId::new(artifact_id("u")),
        purpose,
        media_type: media_type.into(),
        size: 1,
    }
}

#[test]
fn names_follow_node_tool_artifacts() {
    assert_eq!(segment("sess_a/b"), "sess_a_b");
    assert_eq!(segment("😀"), "__");
    assert_eq!(segment(""), "unknown");
    let paths = StoragePaths::resolve(
        &StorageSettings {
            session_db_path: None,
            storage_dir: Some("/data"),
        },
        Path::new("/home"),
        Path::new("/"),
    );
    let at = |purpose, media_type| {
        path(paths.artifacts(), &artifact("sess 1", purpose, media_type))
            .as_path()
            .to_path_buf()
    };
    assert_eq!(
        at(ArtifactPurpose::ToolOutput, "text/plain; charset=utf-8"),
        Path::new("/data/cli/artifacts/sess_1/tool_output-tool-result-u.txt")
    );
    assert_eq!(
        at(ArtifactPurpose::Media, "image/JPEG"),
        Path::new("/data/cli/artifacts/sess_1/media-tool-result-u.jpg")
    );
    // 未知的二进制类型用 .bin：Node 按 base64 读回，不会当成 JSON 文本。
    assert_eq!(
        at(ArtifactPurpose::Attachment, "application/octet-stream"),
        Path::new("/data/cli/artifacts/sess_1/attachment-tool-result-u.bin")
    );
    assert_eq!(
        uri(&artifact(
            "sess 1/é",
            ArtifactPurpose::Checkpoint,
            "application/json"
        )),
        "zcode-artifact://sess%201%2F%C3%A9/tool-result-u"
    );
}
