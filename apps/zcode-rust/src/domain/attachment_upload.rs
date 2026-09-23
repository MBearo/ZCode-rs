use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const MAX_BYTES: usize = 20 * 1024 * 1024;
pub const CHUNK_BYTES: usize = 512 * 1024;
const TTL: u64 = 300_000;
#[derive(Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Begin {
    pub connection_id: String,
    pub session_id: String,
    pub upload_id: String,
    pub file_name: String,
    pub mime: String,
    pub total_bytes: usize,
    pub total_chunks: usize,
    pub checksum: String,
}
pub type Key = (String, String, String);
pub struct Upload {
    pub meta: Begin,
    pub chunks: Vec<Vec<u8>>,
    pub bytes: usize,
    pub expires: u64,
    pub committed: Option<String>,
}
#[derive(Default)]
pub struct Uploads(pub BTreeMap<Key, Upload>);
pub fn key(p: &Value) -> Result<Key> {
    let get = |k| {
        p[k].as_str()
            .filter(|s| !s.is_empty() && !s.contains('\0'))
            .map(str::to_owned)
            .context("Invalid upload identity")
    };
    Ok((get("connectionId")?, get("sessionId")?, get("uploadId")?))
}
pub fn valid_mime(mime: &str) -> bool {
    let valid = |s: &str| {
        !s.is_empty()
            && s.as_bytes()[0].is_ascii_alphanumeric()
            && s.bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"!#$&^_.+-".contains(&c))
    };
    mime.len() <= 255
        && mime
            .split_once('/')
            .is_some_and(|(a, b)| valid(a) && valid(b))
}
impl Uploads {
    pub fn prune(&mut self, now: u64) {
        self.0.retain(|_, u| u.expires > now);
    }
    pub fn clear_connection(&mut self, connection: &str) {
        self.0.retain(|k, _| k.0 != connection);
    }
    pub fn begin(&mut self, p: &Value, now: u64) -> Result<Value> {
        self.prune(now);
        let key = key(p)?;
        let meta: Begin = serde_json::from_value(p.clone())?;
        ensure!(
            meta.upload_id.len() <= 128
                && meta.upload_id.as_bytes()[0].is_ascii_alphanumeric()
                && meta
                    .upload_id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"._:-".contains(&c)),
            "Invalid upload id"
        );
        ensure!(
            !meta.file_name.is_empty()
                && meta.file_name.chars().count() <= 255
                && !meta.file_name.contains(['\0', '\r', '\n'])
                && valid_mime(&meta.mime),
            "Invalid attachment metadata"
        );
        ensure!(
            meta.total_bytes <= MAX_BYTES
                && meta.total_chunks <= 64
                && (meta.total_bytes == 0) == (meta.total_chunks == 0),
            "Invalid attachment size"
        );
        ensure!(
            meta.checksum.len() == 71
                && meta.checksum.starts_with("sha256:")
                && meta.checksum[7..]
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
            "Invalid checksum"
        );
        if let Some(upload) = self.0.get_mut(&key) {
            ensure!(upload.meta == meta, "fault.attachment.beginConflict");
            if let Some(reference) = &upload.committed {
                return Ok(
                    json!({"uploadId":meta.upload_id,"state":"committed","nextChunkIndex":meta.total_chunks,"ref":reference}),
                );
            }
            upload.expires = now + TTL;
            return Ok(
                json!({"uploadId":meta.upload_id,"state":"staging","nextChunkIndex":upload.chunks.len()}),
            );
        }
        ensure!(
            self.0.values().filter(|u| u.committed.is_none()).count() < 16,
            "fault.attachment.tooManyUploads"
        );
        ensure!(
            meta.total_bytes <= meta.total_chunks * CHUNK_BYTES,
            "fault.attachment.chunkCountInsufficient"
        );
        let result = json!({"uploadId":meta.upload_id,"state":"staging","nextChunkIndex":0});
        self.0.insert(
            key,
            Upload {
                meta,
                chunks: vec![],
                bytes: 0,
                expires: now + TTL,
                committed: None,
            },
        );
        Ok(result)
    }
    pub fn chunk(&mut self, p: &Value, now: u64) -> Result<Value> {
        use base64::Engine as _;
        self.prune(now);
        let key = key(p)?;
        let total: usize = self.0.values().map(|u| u.bytes).sum();
        let upload = self
            .0
            .get_mut(&key)
            .filter(|u| u.committed.is_none())
            .context("fault.attachment.uploadNotFound")?;
        let index = p["chunkIndex"].as_u64().context("Invalid chunk index")? as usize;
        let encoded = p["dataBase64"].as_str().context("Invalid chunk bytes")?;
        ensure!(
            encoded.len() <= CHUNK_BYTES.div_ceil(3) * 4,
            "Attachment chunk too large"
        );
        let bytes = base64::engine::general_purpose::STANDARD.decode(encoded)?;
        ensure!(bytes.len() <= CHUNK_BYTES, "Attachment chunk too large");
        if index < upload.chunks.len() {
            ensure!(
                upload.chunks[index] == bytes,
                "fault.attachment.chunkConflict"
            );
        } else {
            ensure!(index == upload.chunks.len(), "fault.attachment.chunkGap");
            ensure!(
                index < upload.meta.total_chunks,
                "fault.attachment.tooManyChunks"
            );
            ensure!(!bytes.is_empty(), "fault.attachment.emptyChunk");
            ensure!(
                upload.bytes + bytes.len() <= upload.meta.total_bytes,
                "fault.attachment.totalBytesExceeded"
            );
            ensure!(
                total + bytes.len() <= 64 * 1024 * 1024,
                "fault.attachment.stagingCapacityExceeded"
            );
            upload.bytes += bytes.len();
            upload.chunks.push(bytes);
            upload.expires = now + TTL;
        }
        Ok(json!({"uploadId":key.2,"nextChunkIndex":upload.chunks.len()}))
    }
    pub fn validated(&self, key: &Key) -> Result<&Upload> {
        let upload = self.0.get(key).context("fault.attachment.uploadNotFound")?;
        if upload.committed.is_none() {
            ensure!(
                upload.bytes == upload.meta.total_bytes
                    && upload.chunks.len() == upload.meta.total_chunks,
                "fault.attachment.uploadIncomplete"
            );
            let mut hash = Sha256::new();
            for chunk in &upload.chunks {
                hash.update(chunk);
            }
            ensure!(
                format!("sha256:{:x}", hash.finalize()) == upload.meta.checksum,
                "fault.attachment.checksumMismatch"
            );
        }
        Ok(upload)
    }
    pub fn committed(&mut self, key: &Key, reference: String, now: u64) {
        let upload = self.0.get_mut(key).unwrap();
        upload.chunks.clear();
        upload.bytes = 0;
        upload.committed = Some(reference);
        upload.expires = now + TTL;
    }
}
