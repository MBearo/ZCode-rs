//! Node's tool artifact files (`NodeToolArtifactStore`):
//! `zcode-artifact://<session>/<artifact>` names the file in
//! `<root>/<sanitized session>` whose name contains the artifact id.
use crate::domain::session::StoredAttachment;
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

/// JS `encodeURIComponent`.
fn encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Node `writeToolResultArtifact` of a prompt attachment (`writePromptAttachment`,
/// `persistAttachmentDataUrl`): the bytes as a `data:` URL text artifact named
/// after `call`. Returns its URI and the stored attachment.
pub async fn write_data_url(
    root: &Path,
    session: &str,
    call: &str,
    chunks: &[Vec<u8>],
    mime: &str,
) -> anyhow::Result<(String, StoredAttachment)> {
    // 大小上限由调用方把关（上传、本地快照、工具媒体各有 Node 的限制）。
    let total: usize = chunks.iter().map(Vec::len).sum();
    let mut bytes = Vec::with_capacity(total);
    for chunk in chunks {
        bytes.extend_from_slice(chunk);
    }
    let content = format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    );
    let artifact = format!("tool-result-{}", crate::id());
    let dir = root.join(segment(session));
    tokio::fs::create_dir_all(&dir).await?;
    let path = dir.join(format!("{}-{artifact}.txt", segment(call)));
    // 临时名不含 artifact id：Node 按 id 子串定位文件，写到一半的文件不能被读到。
    let temp = dir.join(format!(".{}.tmp", crate::id()));
    let write = async {
        tokio::fs::write(&temp, content.as_bytes()).await?;
        tokio::fs::rename(&temp, &path).await
    };
    if let Err(error) = write.await {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(error.into());
    }
    let uri = format!("zcode-artifact://{}/{}", encode(session), encode(&artifact));
    let stored = StoredAttachment {
        path: path.to_string_lossy().into_owned(),
        media_type: mime.into(),
        total_bytes: total as u64,
        data_url: true,
        ..Default::default()
    };
    Ok((uri, stored))
}

/// The prompt attachment behind `uri`: a base64 `data:` URL artifact.
pub async fn stored(root: &Path, uri: &str) -> anyhow::Result<Option<StoredAttachment>> {
    let Some((session, artifact)) = parse(uri) else {
        return Ok(None);
    };
    let dir = root.join(segment(&session));
    let mut entries = match tokio::fs::read_dir(&dir).await {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.contains(&artifact) || !text_file(&name) {
            continue;
        }
        let path = entry.path();
        let Some((mime, header)) = data_url_header(&path).await? else {
            return Ok(None);
        };
        let payload = tokio::fs::metadata(&path).await?.len() - header;
        let mut tail = [0u8; 2];
        if payload >= 2 {
            use tokio::io::{AsyncReadExt, AsyncSeekExt};
            let mut file = tokio::fs::File::open(&path).await?;
            file.seek(std::io::SeekFrom::End(-2)).await?;
            file.read_exact(&mut tail).await?;
        }
        let padding = tail.iter().filter(|b| **b == b'=').count() as u64;
        return Ok(Some(StoredAttachment {
            path: path.to_string_lossy().into_owned(),
            media_type: mime,
            total_bytes: (payload / 4 * 3).saturating_sub(padding),
            data_url: true,
            ..Default::default()
        }));
    }
    Ok(None)
}

/// The media type and header length (through the comma) of a base64 `data:`
/// URL file.
pub async fn data_url_header(path: &Path) -> anyhow::Result<Option<(String, u64)>> {
    use tokio::io::AsyncReadExt;
    let mut head = Vec::with_capacity(256);
    tokio::fs::File::open(path)
        .await?
        .take(256)
        .read_to_end(&mut head)
        .await?;
    let Some(comma) = head.iter().position(|b| *b == b',') else {
        return Ok(None);
    };
    let header = String::from_utf8_lossy(&head[..comma]).to_lowercase();
    let Some(mime) = header
        .strip_prefix("data:")
        .and_then(|h| h.strip_suffix(";base64"))
    else {
        return Ok(None);
    };
    Ok(Some((mime.to_owned(), comma as u64 + 1)))
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
