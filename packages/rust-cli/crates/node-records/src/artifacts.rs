//! Artifact 的 URI `zcode-artifact://<sessionId>/<artifactId>`（docs/specs/rust-m11-node-storage.md §2.1）：写进 tool part 与
//! attachment 的引用。文件本身（目录、文件名、读写）在 node-store。

use rust_cli_kernel_api::persisted_output::ARTIFACT_SCHEME;
use rust_cli_kernel_api::store::ArtifactRef;

/// JS `encodeURIComponent`。
fn encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Artifact 的 URI（写进 part 时使用）。
pub fn uri(artifact: &ArtifactRef) -> String {
    format!(
        "{ARTIFACT_SCHEME}{}/{}",
        encode(artifact.session.as_str()),
        encode(artifact.id.as_str())
    )
}
