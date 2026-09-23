use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::path::Path;
fn resolved(p: &Value, cwd: &str, artifacts: &Path) -> Result<String> {
    use base64::Engine as _;
    let mime = p["mime"].as_str().unwrap_or("application/octet-stream");
    let original = p["url"].as_str().context("Missing legacy attachment URL")?;
    let artifact = if original.starts_with("zcode-artifact:") {
        let parsed = reqwest::Url::parse(original)?;
        let decode = |s: &str| -> Result<String> {
            let bytes = reqwest::Url::parse(&format!("file:///{s}"))?
                .to_file_path()
                .map_err(|_| anyhow::anyhow!("Invalid artifact identity"))?;
            Ok(bytes.to_string_lossy().trim_start_matches('/').to_owned())
        };
        let session = decode(parsed.host_str().context("Missing artifact session")?)?;
        let id = decode(parsed.path().trim_start_matches('/'))?;
        ensure!(
            !id.is_empty() && !id.contains(['/', '\\']),
            "Invalid artifact id"
        );
        let safe: String = session
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || "._-".contains(c) {
                    c
                } else {
                    '_'
                }
            })
            .take(120)
            .collect();
        ensure!(safe != "." && safe != "..", "Invalid artifact session");
        let entry = std::fs::read_dir(artifacts.join(safe))?
            .filter_map(Result::ok)
            .find(|e| e.file_name().to_string_lossy().contains(&id))
            .context("Legacy artifact missing")?;
        ensure!(
            entry.metadata()?.len() <= 20 * 1024 * 1024,
            "Legacy artifact exceeds migration budget"
        );
        let data = std::fs::read(entry.path())?;
        // TS media artifacts 保存完整 data URL；二进制产物保存原始字节。
        if data.starts_with(b"data:") {
            String::from_utf8(data)?
        } else {
            format!(
                "data:{mime};base64,{}",
                base64::engine::general_purpose::STANDARD.encode(data)
            )
        }
    } else {
        original.to_owned()
    };
    let url = artifact.as_str();
    let mut resolved = url.to_owned();
    if url.starts_with("file:") || (!url.contains("://") && !url.starts_with("data:")) {
        let path = if url.starts_with("file:") {
            reqwest::Url::parse(url)?
                .to_file_path()
                .map_err(|_| anyhow::anyhow!("Invalid attachment path"))?
        } else {
            Path::new(cwd).join(url)
        };
        let metadata = std::fs::metadata(&path)
            .context("Legacy attachment missing; original history was preserved")?;
        ensure!(
            metadata.len() <= 20 * 1024 * 1024,
            "Legacy attachment exceeds migration budget"
        );
        let bytes = std::fs::read(path)?;
        resolved = format!(
            "data:{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        );
    }
    Ok(resolved)
}
pub(super) fn file_content(p: &Value, cwd: &str, artifacts: &Path) -> Result<Vec<Value>> {
    use base64::Engine as _;
    if let Some(text) = p["source"]["text"]["value"].as_str() {
        return Ok(vec![json!({"type":"text","text":text})]);
    }
    let mime = p["mime"].as_str().unwrap_or("application/octet-stream");
    let resolved = resolved(p, cwd, artifacts)?;
    let url = resolved.as_str();
    if mime.starts_with("image/") {
        return Ok(vec![
            json!({"type":"image_url","image_url":{"url":resolved}}),
        ]);
    }
    if mime.starts_with("text/")
        && let Some(preview) = p["metadata"]["preview"]["text"].as_str()
    {
        return Ok(vec![json!({"type":"text","text":preview})]);
    }
    if mime.starts_with("text/")
        && let Some(data) = url.strip_prefix("data:")
    {
        let (_, data) = data
            .split_once(";base64,")
            .context("Unsupported text data URL")?;
        return Ok(vec![
            json!({"type":"text","text":String::from_utf8(base64::engine::general_purpose::STANDARD.decode(data)?)?}),
        ]);
    }
    ensure!(
        mime == "application/pdf",
        "Unsupported legacy attachment MIME; original history was preserved"
    );
    Ok(vec![
        json!({"type":"file","file":{"filename":p["filename"].as_str().unwrap_or("attachment.pdf"),"file_data":resolved}}),
    ])
}

// 导入侧把可用附件字节快照进 Rust 目录；重启和回退均不依赖 TS 后续的缓存清理。
pub(super) fn snapshot(
    p: &Value,
    cwd: &str,
    artifacts: &Path,
    dir: &Path,
) -> Result<Option<crate::domain::session::StoredAttachment>> {
    use base64::Engine as _;
    let data = resolved(p, cwd, artifacts)?;
    let Some(data) = data.strip_prefix("data:") else {
        return Ok(None);
    };
    let (mime, data) = data
        .split_once(";base64,")
        .context("Unsupported attachment data URL")?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(data)?;
    Ok(Some(snapshot_bytes(mime, &bytes, dir)?))
}
pub(super) fn snapshot_bytes(
    mime: &str,
    bytes: &[u8],
    dir: &Path,
) -> Result<crate::domain::session::StoredAttachment> {
    use sha2::{Digest, Sha256};
    ensure!(
        bytes.len() <= 20 * 1024 * 1024,
        "Attachment snapshot exceeds limit"
    );
    let root = dir.join("imported-attachments");
    std::fs::create_dir_all(&root)?;
    let path = root.join(format!("{:x}", Sha256::digest(bytes)));
    if !path.exists() {
        let temp = root.join(super::id());
        std::fs::write(&temp, bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::File::open(&temp)?.sync_all()?;
        std::fs::rename(temp, &path)?;
        #[cfg(unix)]
        std::fs::File::open(&root)?.sync_all()?;
    }
    Ok(crate::domain::session::StoredAttachment {
        path: path.to_string_lossy().into_owned(),
        source_path: None,
        media_type: mime.into(),
        total_bytes: bytes.len() as u64,
    })
}
