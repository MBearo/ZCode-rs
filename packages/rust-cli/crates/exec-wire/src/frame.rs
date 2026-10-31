//! 帧的内容与分片（docs/specs/rust-cli/remote-exec.md §18.2）。
//!
//! 一帧是 `u8 类型 | u32 BE 头的长度 | JSON 头 | 二进制段`。字节流传输在帧前面加的 u32 长度不在这里，归
//! `exec-transport`。编码后超过 [`PART_SIZE`] 的消息拆成 `part` 帧（`0x30 | u64 BE 消息号 | u8 是否最后 | 一片`），
//! 收方拼回原来的帧再解码。

use std::collections::HashMap;

use bytes::{BufMut, Bytes, BytesMut};
use rust_cli_base::units;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::blob::Body;

/// 一片的大小：超过它的消息拆开发，片与片之间可以插控制帧。
pub const PART_SIZE: usize = 256 * units::KIB;
/// 一条消息（拼回之后）的上限。
pub const MAX_MESSAGE: u64 = 512 * units::MIB as u64;

const HEAD: usize = 1 + 4;
const PART_HEAD: usize = 1 + 8 + 1;

// 帧的类型字节（§18.2 的表）：`Kind` 的判别值就是它们，编码与解码都只经 `Kind`。
const HELLO: u8 = 0x01;
const WELCOME: u8 = 0x02;
const REFUSE: u8 = 0x03;
const REQUEST: u8 = 0x10;
const REPLY: u8 = 0x11;
const EVENT: u8 = 0x12;
const ACK: u8 = 0x13;
const CLOSE: u8 = 0x14;
const PING: u8 = 0x20;
const PONG: u8 = 0x21;
/// 大消息的一片（不是 `Kind`：收方拼回原来的帧再解码）。
const PART: u8 = 0x30;
const ADMIN: u8 = 0x40;
const ADMIN_REPLY: u8 = 0x41;

/// 帧的类型（§18.2 的表）：判别值是线上的类型字节。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Kind {
    Hello = HELLO,
    Welcome = WELCOME,
    Refuse = REFUSE,
    Request = REQUEST,
    Reply = REPLY,
    Event = EVENT,
    Ack = ACK,
    Close = CLOSE,
    Ping = PING,
    Pong = PONG,
    /// 管理帧（§19.6）：连接的第一帧，代替 `hello`。
    Admin = ADMIN,
    AdminReply = ADMIN_REPLY,
}

impl Kind {
    /// 全部类型（解码按类型字节在这里找）。
    const ALL: [Self; 12] = [
        Self::Hello,
        Self::Welcome,
        Self::Refuse,
        Self::Request,
        Self::Reply,
        Self::Event,
        Self::Ack,
        Self::Close,
        Self::Ping,
        Self::Pong,
        Self::Admin,
        Self::AdminReply,
    ];

    pub fn code(self) -> u8 {
        self as u8
    }

    pub fn from_code(code: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.code() == code)
    }

    /// 控制帧（心跳、ack、关闭流）：发送方在大消息的两片之间先发它们。
    pub fn is_control(self) -> bool {
        matches!(self, Self::Ack | Self::Close | Self::Ping | Self::Pong)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    #[error("frame is truncated")]
    Truncated,
    #[error("frame header is not valid: {0}")]
    Header(String),
    #[error("message of {size} bytes exceeds the {max}-byte limit")]
    TooLarge { size: u64, max: u64 },
}

/// 一帧：类型、JSON 头、二进制段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub kind: Kind,
    pub header: Bytes,
    pub body: Body,
}

impl Frame {
    /// 编码成一段字节（还没分片）。
    pub fn encode<T: Serialize>(kind: Kind, header: &T, body: &Body) -> Result<Bytes, FrameError> {
        let header =
            serde_json::to_vec(header).map_err(|error| FrameError::Header(error.to_string()))?;
        let size = (HEAD + header.len() + body.len()) as u64;
        if size > MAX_MESSAGE {
            return Err(FrameError::TooLarge {
                size,
                max: MAX_MESSAGE,
            });
        }
        let header_len = u32::try_from(header.len()).map_err(|_| FrameError::TooLarge {
            size,
            max: MAX_MESSAGE,
        })?;
        let mut out = BytesMut::with_capacity(size as usize);
        out.put_u8(kind.code());
        out.put_u32(header_len);
        out.put_slice(&header);
        out.put_slice(body.bytes());
        Ok(out.freeze())
    }

    /// 解开 JSON 头。
    pub fn header<T: DeserializeOwned>(&self) -> Result<T, FrameError> {
        serde_json::from_slice(&self.header).map_err(|error| FrameError::Header(error.to_string()))
    }
}

/// 收到的一段字节解出来的东西。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decoded {
    Frame(Frame),
    Part {
        message: u64,
        last: bool,
        chunk: Bytes,
    },
    /// 不认识的类型（新类型只在对方报告了能力位时才会发）：丢弃。
    Unknown(u8),
}

/// 解一段字节（一帧或一片）。
pub fn decode(payload: Bytes) -> Result<Decoded, FrameError> {
    let Some(&code) = payload.first() else {
        return Err(FrameError::Truncated);
    };
    if code == PART {
        if payload.len() < PART_HEAD {
            return Err(FrameError::Truncated);
        }
        let mut id = [0u8; 8];
        id.copy_from_slice(&payload[1..9]);
        return Ok(Decoded::Part {
            message: u64::from_be_bytes(id),
            last: payload[9] != 0,
            chunk: payload.slice(PART_HEAD..),
        });
    }
    let Some(kind) = Kind::from_code(code) else {
        return Ok(Decoded::Unknown(code));
    };
    if payload.len() < HEAD {
        return Err(FrameError::Truncated);
    }
    let mut len = [0u8; 4];
    len.copy_from_slice(&payload[1..HEAD]);
    let end = HEAD + u32::from_be_bytes(len) as usize;
    if payload.len() < end {
        return Err(FrameError::Truncated);
    }
    Ok(Decoded::Frame(Frame {
        kind,
        header: payload.slice(HEAD..end),
        body: Body::new(payload.slice(end..)),
    }))
}

/// 发送方的分片：消息号在一个方向上递增。
#[derive(Debug)]
pub struct Splitter {
    next: u64,
    part_size: usize,
}

impl Default for Splitter {
    fn default() -> Self {
        Self::new(PART_SIZE)
    }
}

impl Splitter {
    pub fn new(part_size: usize) -> Self {
        Self {
            next: 1,
            part_size: part_size.max(1),
        }
    }

    /// 不超过一片的原样返回；否则拆成 `part` 帧。
    pub fn split(&mut self, payload: Bytes) -> Vec<Bytes> {
        if payload.len() <= self.part_size {
            return vec![payload];
        }
        let id = self.next;
        self.next += 1;
        let count = payload.len().div_ceil(self.part_size);
        (0..count)
            .map(|index| {
                let start = index * self.part_size;
                let end = (start + self.part_size).min(payload.len());
                let mut part = BytesMut::with_capacity(PART_HEAD + end - start);
                part.put_u8(PART);
                part.put_u64(id);
                part.put_u8(u8::from(index + 1 == count));
                part.put_slice(&payload[start..end]);
                part.freeze()
            })
            .collect()
    }
}

/// 收方的拼装：`part` 按消息号拼回，最后一片到了再解码。
#[derive(Debug)]
pub struct Reassembler {
    partial: HashMap<u64, BytesMut>,
    max_message: u64,
}

impl Default for Reassembler {
    fn default() -> Self {
        Self::new(MAX_MESSAGE)
    }
}

/// [`Reassembler::push`] 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Received {
    Frame(Frame),
    /// 一条消息的中间一片，还没拼完。
    Pending,
    Unknown(u8),
}

impl Reassembler {
    pub fn new(max_message: u64) -> Self {
        Self {
            partial: HashMap::new(),
            max_message,
        }
    }

    pub fn push(&mut self, payload: Bytes) -> Result<Received, FrameError> {
        match decode(payload)? {
            Decoded::Frame(frame) => Ok(Received::Frame(frame)),
            Decoded::Unknown(code) => Ok(Received::Unknown(code)),
            Decoded::Part {
                message,
                last,
                chunk,
            } => {
                let buffer = self.partial.entry(message).or_default();
                let size = (buffer.len() + chunk.len()) as u64;
                if size > self.max_message {
                    self.partial.remove(&message);
                    return Err(FrameError::TooLarge {
                        size,
                        max: self.max_message,
                    });
                }
                buffer.put_slice(&chunk);
                if !last {
                    return Ok(Received::Pending);
                }
                let whole = self.partial.remove(&message).unwrap_or_default().freeze();
                match decode(whole)? {
                    Decoded::Frame(frame) => Ok(Received::Frame(frame)),
                    Decoded::Unknown(code) => Ok(Received::Unknown(code)),
                    // 片里不能再套片。
                    Decoded::Part { .. } => Err(FrameError::Header("nested part".to_owned())),
                }
            }
        }
    }
}
