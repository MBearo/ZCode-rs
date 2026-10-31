//! WebSocket 客户端（Monitor 的 ws 来源，docs/specs/rust-cli/tools-more.md §19.7）。
//!
//! 升级请求经出口的同一个 HTTP 客户端发出（代理、不走代理的规则与根证书同其他请求）：`ws://` 换成 `http://`、
//! `wss://` 换成 `https://`，响应必须是 101 且 `Sec-WebSocket-Accept` 对得上。之后按 RFC 6455 读帧：文本帧（分片
//! 拼起来）与二进制帧交给调用方，ping 回 pong，close 回 close；单帧（或拼起来的消息）超过 [`MAX_PAYLOAD`] 时报错。
//! 客户端发的帧都加掩码。

use base64::Engine;
use reqwest::header::HeaderMap;
use rust_cli_base::proto::http;
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};

use crate::http::header_pair;
use crate::{HeaderValue, NetError};

/// 一帧（或一条分片的消息）最多这么多字节。
pub const MAX_PAYLOAD: u64 = 1_048_576;

const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// 关闭码（RFC 6455 §7.4.1）：正常关闭。
pub const CLOSE_NORMAL: u16 = 1000;
/// 关闭码：close 帧里没有状态码（只在本地报告，不上线）。
pub const CLOSE_NO_STATUS: u16 = 1005;
/// 关闭码：没有 close 帧就断开（只在本地报告，不上线）。
pub const CLOSE_ABNORMAL: u16 = 1006;

/// 帧的 opcode（RFC 6455 §5.2）。
mod opcode {
    /// 续帧（分片消息的后续部分）。
    pub(super) const CONTINUATION: u8 = 0x0;
    pub(super) const TEXT: u8 = 0x1;
    pub(super) const BINARY: u8 = 0x2;
    pub(super) const CLOSE: u8 = 0x8;
    pub(super) const PING: u8 = 0x9;
    pub(super) const PONG: u8 = 0xa;
}

/// 帧头第一个字节的 FIN 位（消息的最后一帧）。
const FIN_BIT: u8 = 0x80;
/// 帧头第一个字节里 opcode 占的位。
const OPCODE_BITS: u8 = 0x0f;
/// 帧头第二个字节的 MASK 位（内容加了掩码）。
const MASK_BIT: u8 = 0x80;
/// 帧头第二个字节里长度占的位。
const LENGTH_BITS: u8 = 0x7f;
/// 长度标记：真正的长度在后面的 2 字节（大端）。
const LENGTH_16: u8 = 126;
/// 长度标记：真正的长度在后面的 8 字节（大端）。
const LENGTH_64: u8 = 127;
/// 掩码键的字节数。
const MASK_KEY_BYTES: usize = 4;
/// 握手的 `Sec-WebSocket-Key` 是这么多随机字节的 base64（RFC 6455 §4.1）。
const HANDSHAKE_KEY_BYTES: usize = 16;
/// close 帧内容开头的状态码的字节数（大端），后面是原因。
const CLOSE_CODE_BYTES: usize = 2;

/// 读到的一帧。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsFrame {
    Text(String),
    /// 二进制帧（只报告字节数）。
    Binary(usize),
    /// 关闭：close 帧的代码与原因；没有 close 帧就断开时是 [`CLOSE_ABNORMAL`]、原因为空。
    Close {
        code: u16,
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WsError {
    /// 升级请求的响应不是 101。
    #[error("Unexpected server response: {status}")]
    Rejected { status: u16 },
    #[error(transparent)]
    Net(#[from] NetError),
    /// 握手或帧不合协议。
    #[error("{0}")]
    Protocol(String),
    /// 一帧或一条消息超过 [`MAX_PAYLOAD`]（字节数）。
    #[error("frame of {0} bytes exceeds the limit")]
    TooBig(u64),
}

/// 一条打开的连接。
pub struct WebSocket {
    reader: ReadHalf<reqwest::Upgraded>,
    writer: WriteHalf<reqwest::Upgraded>,
    /// 已经发过 close 帧。
    closing: bool,
}

/// `ws:` / `wss:` 换成 HTTP 的写法。
fn http_url(url: &str) -> Result<String, WsError> {
    let mut parsed =
        reqwest::Url::parse(url).map_err(|error| WsError::Protocol(error.to_string()))?;
    let scheme = match parsed.scheme() {
        "ws" | "http" => "http",
        "wss" | "https" => "https",
        other => {
            return Err(WsError::Protocol(format!(
                "unsupported socket base scheme {other}:"
            )));
        }
    };
    parsed
        .set_scheme(scheme)
        .map_err(|()| WsError::Protocol("invalid URL".to_owned()))?;
    Ok(parsed.to_string())
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0_u8; N];
    for chunk in bytes.chunks_mut(size_of::<u64>()) {
        let value = rust_cli_base::random::u64().to_le_bytes();
        chunk.copy_from_slice(&value[..chunk.len()]);
    }
    bytes
}

/// 测试的服务端回握手用（env-local 的 ws 测试）。
#[doc(hidden)]
pub fn accept_for_tests(key: &str) -> String {
    accept_key(key)
}

/// `Sec-WebSocket-Accept`：`base64(sha1(key + GUID))`。
pub(crate) fn accept_key(key: &str) -> String {
    base64::engine::general_purpose::STANDARD
        .encode(crate::sha1::digest(format!("{key}{GUID}").as_bytes()))
}

/// 发升级请求、核对响应（`Egress::websocket` 调用）。
pub(crate) async fn connect(
    client: &reqwest::Client,
    base: HeaderMap,
    url: &str,
    protocols: &[String],
) -> Result<WebSocket, WsError> {
    let target = http_url(url)?;
    let key =
        base64::engine::general_purpose::STANDARD.encode(random_bytes::<HANDSHAKE_KEY_BYTES>());
    let mut headers = base;
    let mut set = |name: &str, value: String| -> Result<(), WsError> {
        let (name, value) =
            header_pair(name, &HeaderValue::Plain(value)).map_err(WsError::Protocol)?;
        headers.insert(name, value);
        Ok(())
    };
    set("connection", "Upgrade".to_owned())?;
    set("upgrade", "websocket".to_owned())?;
    set("sec-websocket-version", "13".to_owned())?;
    set("sec-websocket-key", key.clone())?;
    if !protocols.is_empty() {
        set("sec-websocket-protocol", protocols.join(", "))?;
    }
    let response = client
        .get(&target)
        .headers(headers)
        .send()
        .await
        .map_err(|error| NetError::from_reqwest(&error))?;
    let status = response.status().as_u16();
    if status != http::SWITCHING_PROTOCOLS {
        return Err(WsError::Rejected { status });
    }
    let accept = response
        .headers()
        .get("sec-websocket-accept")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if accept != accept_key(&key) {
        return Err(WsError::Protocol(
            "Invalid Sec-WebSocket-Accept header".to_owned(),
        ));
    }
    let upgraded = response
        .upgrade()
        .await
        .map_err(|error| NetError::from_reqwest(&error))?;
    let (reader, writer) = tokio::io::split(upgraded);
    Ok(WebSocket {
        reader,
        writer,
        closing: false,
    })
}

/// 一个客户端帧（FIN、掩码）。
pub(crate) fn client_frame(opcode: u8, payload: &[u8], mask: [u8; MASK_KEY_BYTES]) -> Vec<u8> {
    let mut frame = vec![FIN_BIT | opcode];
    let length = payload.len();
    if length < usize::from(LENGTH_16) {
        frame.push(MASK_BIT | u8::try_from(length).unwrap_or(0));
    } else if let Ok(short) = u16::try_from(length) {
        frame.push(MASK_BIT | LENGTH_16);
        frame.extend_from_slice(&short.to_be_bytes());
    } else {
        frame.push(MASK_BIT | LENGTH_64);
        frame.extend_from_slice(&(length as u64).to_be_bytes());
    }
    frame.extend_from_slice(&mask);
    frame.extend(
        payload
            .iter()
            .enumerate()
            .map(|(index, byte)| byte ^ mask[index % MASK_KEY_BYTES]),
    );
    frame
}

fn eof(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::UnexpectedEof
}

fn io(error: &std::io::Error) -> WsError {
    WsError::Net(NetError::Io(error.to_string()))
}

impl WebSocket {
    async fn send(&mut self, opcode: u8, payload: &[u8]) -> Result<(), WsError> {
        let frame = client_frame(opcode, payload, random_bytes::<MASK_KEY_BYTES>());
        self.writer
            .write_all(&frame)
            .await
            .map_err(|error| io(&error))?;
        self.writer.flush().await.map_err(|error| io(&error))
    }

    /// 读一帧：（FIN、opcode、内容）。
    async fn frame(&mut self) -> Result<(bool, u8, Vec<u8>), std::io::Error> {
        let mut head = [0_u8; 2];
        self.reader.read_exact(&mut head).await?;
        let fin = head[0] & FIN_BIT != 0;
        let opcode = head[0] & OPCODE_BITS;
        let masked = head[1] & MASK_BIT != 0;
        let mut length = u64::from(head[1] & LENGTH_BITS);
        if length == u64::from(LENGTH_16) {
            length = u64::from(self.reader.read_u16().await?);
        } else if length == u64::from(LENGTH_64) {
            length = self.reader.read_u64().await?;
        }
        if length > MAX_PAYLOAD {
            return Err(std::io::Error::other(TooBig(length)));
        }
        let mut mask = [0_u8; MASK_KEY_BYTES];
        if masked {
            self.reader.read_exact(&mut mask).await?;
        }
        let mut payload = vec![0_u8; usize::try_from(length).unwrap_or(0)];
        self.reader.read_exact(&mut payload).await?;
        if masked {
            for (index, byte) in payload.iter_mut().enumerate() {
                *byte ^= mask[index % MASK_KEY_BYTES];
            }
        }
        Ok((fin, opcode, payload))
    }

    /// 下一条消息；ping 回 pong，close 回 close。
    pub async fn next(&mut self) -> Result<WsFrame, WsError> {
        // 分片的消息：（是不是文本、已收到的内容）。
        let mut message: Option<(bool, Vec<u8>)> = None;
        loop {
            let (fin, opcode, payload) = match self.frame().await {
                Ok(frame) => frame,
                Err(error) if eof(&error) => {
                    return Ok(WsFrame::Close {
                        code: CLOSE_ABNORMAL,
                        reason: String::new(),
                    });
                }
                Err(error) => {
                    if let Some(TooBig(length)) = error
                        .get_ref()
                        .and_then(|inner| inner.downcast_ref::<TooBig>())
                    {
                        return Err(WsError::TooBig(*length));
                    }
                    return Err(io(&error));
                }
            };
            // 控制帧可以夹在分片之间，它们的 FIN 不结束消息；只有数据帧与续帧的 FIN 结束。
            let data = match opcode {
                opcode::CONTINUATION => {
                    let Some((_, body)) = message.as_mut() else {
                        return Err(WsError::Protocol(
                            "unexpected continuation frame".to_owned(),
                        ));
                    };
                    body.extend_from_slice(&payload);
                    if body.len() as u64 > MAX_PAYLOAD {
                        return Err(WsError::TooBig(body.len() as u64));
                    }
                    true
                }
                opcode::TEXT | opcode::BINARY if message.is_some() => {
                    return Err(WsError::Protocol(
                        "expected a continuation frame".to_owned(),
                    ));
                }
                opcode::TEXT | opcode::BINARY => {
                    message = Some((opcode == opcode::TEXT, payload));
                    true
                }
                opcode::CLOSE => {
                    let code = match payload.as_slice() {
                        [high, low, ..] => u16::from_be_bytes([*high, *low]),
                        _ => CLOSE_NO_STATUS,
                    };
                    let reason = String::from_utf8_lossy(
                        payload.get(CLOSE_CODE_BYTES..).unwrap_or_default(),
                    )
                    .into_owned();
                    if !self.closing {
                        self.closing = true;
                        let echo = &payload[..payload.len().min(CLOSE_CODE_BYTES)];
                        let _ = self.send(opcode::CLOSE, echo).await;
                    }
                    return Ok(WsFrame::Close { code, reason });
                }
                opcode::PING => {
                    self.send(opcode::PONG, &payload).await?;
                    false
                }
                opcode::PONG => false,
                other => return Err(WsError::Protocol(format!("unknown opcode {other}"))),
            };
            if data
                && fin
                && let Some((text, body)) = message.take()
            {
                return Ok(if text {
                    WsFrame::Text(String::from_utf8_lossy(&body).into_owned())
                } else {
                    WsFrame::Binary(body.len())
                });
            }
        }
    }

    /// 发 close 帧（正常关闭 [`CLOSE_NORMAL`]）。
    pub async fn close(&mut self) {
        if !self.closing {
            self.closing = true;
            let _ = self.send(opcode::CLOSE, &CLOSE_NORMAL.to_be_bytes()).await;
        }
    }
}

/// 帧太大（`frame` 的 io 错误里带着它）。
#[derive(Debug)]
struct TooBig(u64);

impl std::fmt::Display for TooBig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "frame of {} bytes", self.0)
    }
}

impl std::error::Error for TooBig {}

#[cfg(test)]
#[path = "websocket_tests.rs"]
mod tests;
