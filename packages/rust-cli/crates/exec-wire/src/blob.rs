//! 二进制段（docs/specs/rust-cli/remote-exec.md §18.2）：字节字段不进 JSON，JSON 里只记它在二进制段里的位置。

use bytes::Bytes;
use serde::{Deserialize, Serialize};

use crate::envelope::WireError;

/// 二进制段里的一段：`at` 是偏移，`len` 是长度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Blob {
    pub at: u64,
    pub len: u64,
}

/// 编码一条消息时攒二进制段：每个字节字段追加在末尾，得到它的 [`Blob`]。
#[derive(Debug, Default)]
pub struct BodyBuilder {
    bytes: Vec<u8>,
}

impl BodyBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, data: &[u8]) -> Blob {
        let at = self.bytes.len() as u64;
        self.bytes.extend_from_slice(data);
        Blob {
            at,
            len: data.len() as u64,
        }
    }

    pub fn finish(self) -> Body {
        Body(Bytes::from(self.bytes))
    }
}

/// 收到的一条消息的二进制段。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Body(Bytes);

impl Body {
    pub fn new(bytes: Bytes) -> Self {
        Self(bytes)
    }

    pub fn empty() -> Self {
        Self(Bytes::new())
    }

    pub fn bytes(&self) -> &Bytes {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// 取 `blob` 指向的字节；越界时是 `bad_request`（对方编码有误）。
    pub fn get(&self, blob: &Blob) -> Result<Bytes, WireError> {
        let start = usize::try_from(blob.at).ok();
        let end = start.and_then(|start| start.checked_add(usize::try_from(blob.len).ok()?));
        match (start, end) {
            (Some(start), Some(end)) if end <= self.0.len() => Ok(self.0.slice(start..end)),
            _ => Err(WireError::BadRequest {
                message: format!(
                    "blob {}+{} is outside the {}-byte body",
                    blob.at,
                    blob.len,
                    self.0.len()
                ),
            }),
        }
    }

    pub fn to_vec(&self, blob: &Blob) -> Result<Vec<u8>, WireError> {
        self.get(blob).map(|bytes| bytes.to_vec())
    }

    /// UTF-8 文字（`read_text` 的内容）；不是 UTF-8 时是 `bad_request`。
    pub fn text(&self, blob: &Blob) -> Result<String, WireError> {
        String::from_utf8(self.to_vec(blob)?).map_err(|_| WireError::BadRequest {
            message: "text blob is not UTF-8".to_owned(),
        })
    }
}
