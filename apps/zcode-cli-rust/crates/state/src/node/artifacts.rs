//! Node's tool artifact files (`NodeToolArtifactStore`):
//! `zcode-artifact://<session>/<artifact>` names the file in
//! `<root>/<sanitized session>` whose name contains the artifact id.
use base64::Engine as _;
use std::path::Path;

/// Node `sanitizePathSegment` (JS regex over UTF-16 code units).
pub fn segment(value: &str) -> String {
    let mut out = String::new();
    for c in value.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            out.push(c);
        } else {
            out.extend(std::iter::repeat_n('_', c.len_utf16()));
        }
    }
    out.truncate(120);
    if out.is_empty() {
        "unknown".into()
    } else {
        out
    }
}

/// JS `decodeURIComponent`; `None` for a malformed escape.
fn decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Node `parseArtifactUri`: the session and artifact ids.
fn parse(uri: &str) -> Option<(String, String)> {
    let url = url::Url::parse(uri).ok()?;
    if url.scheme() != "zcode-artifact" {
        return None;
    }
    let session = decode(url.host_str()?)?;
    let artifact = decode(url.path().trim_start_matches('/'))?;
    (!session.is_empty() && !artifact.is_empty()).then_some((session, artifact))
}

/// Node `contentTypeForFileName` is text (`isTextArtifactContentType`).
fn text_file(name: &str) -> bool {
    let lower = name.to_lowercase();
    [".txt", ".md", ".html", ".htm", ".csv", ".json"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

/// Node `readToolResultArtifact`: text artifacts as UTF-8, others as base64.
pub fn read(root: &Path, uri: &str) -> Option<String> {
    let (session, artifact) = parse(uri)?;
    let dir = root.join(segment(&session));
    let name = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|name| name.contains(&artifact))?;
    let bytes = std::fs::read(dir.join(&name)).ok()?;
    if text_file(&name) {
        Some(String::from_utf8_lossy(&bytes).into_owned())
    } else {
        Some(base64::engine::general_purpose::STANDARD.encode(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifacts_resolve_like_node() {
        assert_eq!(segment("sess_a/b"), "sess_a_b");
        assert_eq!(segment("😀"), "__");
        assert_eq!(segment(""), "unknown");
        let dir = std::env::temp_dir().join(format!("zcode-artifacts-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sess_1")).unwrap();
        std::fs::write(
            dir.join("sess_1/call_1-tool-result-x.json"),
            "data:image/png;base64,AA",
        )
        .unwrap();
        std::fs::write(dir.join("sess_1/call_2-tool-result-y.png"), [1u8, 2]).unwrap();
        assert_eq!(
            read(&dir, "zcode-artifact://sess_1/tool-result-x").as_deref(),
            Some("data:image/png;base64,AA")
        );
        assert_eq!(
            read(&dir, "zcode-artifact://sess_1/tool-result-y").as_deref(),
            Some("AQI=")
        );
        assert_eq!(read(&dir, "zcode-artifact://sess_1/missing"), None);
        assert_eq!(read(&dir, "file:///sess_1/tool-result-x"), None);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
