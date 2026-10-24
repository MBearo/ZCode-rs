//! WebSocket 客户端：握手、文本与二进制帧、分片、ping、关闭、太大的帧、升级被拒（本地的 WebSocket 服务）。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

use super::*;
use crate::{Egress, NetworkPolicy};

/// 读请求头，回 101（`accept` 为 false 时回 403），然后照剧本写帧、读客户端发来的帧。
#[expect(clippy::disallowed_methods, reason = "测试的本地 WebSocket 服务")]
fn serve(script: impl FnOnce(&mut TcpStream) + Send + 'static, accept: bool) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut head = Vec::new();
        let mut byte = [0_u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            head.push(byte[0]);
        }
        let head = String::from_utf8(head).unwrap();
        let key = head
            .lines()
            .find_map(|line| line.strip_prefix("sec-websocket-key: "))
            .unwrap()
            .to_owned();
        assert!(head.contains("sec-websocket-protocol: v1, v2"), "{head}");
        if !accept {
            stream
                .write_all(b"HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\n\r\n")
                .unwrap();
            return;
        }
        let response = format!(
            "HTTP/1.1 101 Switching Protocols\r\nupgrade: websocket\r\nconnection: Upgrade\r\nsec-websocket-accept: {}\r\n\r\n",
            accept_key(&key)
        );
        stream.write_all(response.as_bytes()).unwrap();
        script(&mut stream);
    });
    format!("ws://127.0.0.1:{port}/feed")
}

/// 服务端的帧（不加掩码）。
fn server_frame(fin: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut frame = vec![if fin { 0x80 } else { 0 } | opcode];
    if payload.len() < 126 {
        frame.push(u8::try_from(payload.len()).unwrap());
    } else {
        frame.push(127);
        frame.extend_from_slice(&(payload.len() as u64).to_be_bytes());
    }
    frame.extend_from_slice(payload);
    frame
}

/// 读客户端的一帧（去掉掩码）：（opcode、内容）。
fn client_read(stream: &mut TcpStream) -> (u8, Vec<u8>) {
    let mut head = [0_u8; 2];
    stream.read_exact(&mut head).unwrap();
    assert_eq!(head[1] & 0x80, 0x80, "client frames are masked");
    let length = usize::from(head[1] & 0x7f);
    let mut mask = [0_u8; 4];
    stream.read_exact(&mut mask).unwrap();
    let mut payload = vec![0_u8; length];
    stream.read_exact(&mut payload).unwrap();
    for (index, byte) in payload.iter_mut().enumerate() {
        *byte ^= mask[index % 4];
    }
    (head[0] & 0x0f, payload)
}

async fn egress() -> Egress {
    Egress::new(NetworkPolicy::default()).await.unwrap()
}

fn protocols() -> Vec<String> {
    vec!["v1".to_owned(), "v2".to_owned()]
}

#[test]
fn sha1_matches_the_rfc_example() {
    // RFC 6455 §1.3 的例子。
    assert_eq!(
        accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
        "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
    );
}

#[tokio::test]
async fn frames_are_read_and_pings_answered() {
    let url = serve(
        |stream| {
            stream.write_all(&server_frame(true, 0x1, b"one")).unwrap();
            stream.write_all(&server_frame(false, 0x1, b"tw")).unwrap();
            stream.write_all(&server_frame(true, 0x9, b"hb")).unwrap();
            stream.write_all(&server_frame(true, 0x0, b"o")).unwrap();
            assert_eq!(client_read(stream), (0xa, b"hb".to_vec()));
            stream
                .write_all(&server_frame(true, 0x2, &[0, 1, 2]))
                .unwrap();
            let mut close = 1001_u16.to_be_bytes().to_vec();
            close.extend_from_slice(b"going away");
            stream.write_all(&server_frame(true, 0x8, &close)).unwrap();
            assert_eq!(client_read(stream), (0x8, 1001_u16.to_be_bytes().to_vec()));
        },
        true,
    );
    let mut socket = egress().await.websocket(&url, &protocols()).await.unwrap();
    assert_eq!(
        socket.next().await.unwrap(),
        WsFrame::Text("one".to_owned())
    );
    assert_eq!(
        socket.next().await.unwrap(),
        WsFrame::Text("two".to_owned())
    );
    assert_eq!(socket.next().await.unwrap(), WsFrame::Binary(3));
    assert_eq!(
        socket.next().await.unwrap(),
        WsFrame::Close {
            code: 1001,
            reason: "going away".to_owned()
        }
    );
}

#[tokio::test]
async fn a_dropped_connection_closes_with_1006_and_big_frames_fail() {
    let url = serve(
        |stream| stream.shutdown(std::net::Shutdown::Both).unwrap(),
        true,
    );
    let mut socket = egress().await.websocket(&url, &protocols()).await.unwrap();
    assert_eq!(
        socket.next().await.unwrap(),
        WsFrame::Close {
            code: 1006,
            reason: String::new()
        }
    );
    let url = serve(
        |stream| {
            let big = vec![b'x'; usize::try_from(MAX_PAYLOAD).unwrap() + 1];
            let _ = stream.write_all(&server_frame(true, 0x1, &big));
        },
        true,
    );
    let mut socket = egress().await.websocket(&url, &protocols()).await.unwrap();
    assert_eq!(
        socket.next().await.unwrap_err(),
        WsError::TooBig(MAX_PAYLOAD + 1)
    );
}

#[tokio::test]
async fn a_rejected_upgrade_reports_the_status() {
    let url = serve(|_| {}, false);
    let error = egress()
        .await
        .websocket(&url, &protocols())
        .await
        .err()
        .unwrap();
    assert_eq!(error, WsError::Rejected { status: 403 });
    let error = egress()
        .await
        .websocket("ftp://e.example.com/", &[])
        .await
        .err()
        .unwrap();
    assert!(matches!(error, WsError::Protocol(_)));
}

#[test]
fn client_frames_are_masked() {
    let frame = client_frame(0x1, b"hi", [1, 2, 3, 4]);
    assert_eq!(frame, vec![0x81, 0x82, 1, 2, 3, 4, b'h' ^ 1, b'i' ^ 2]);
}

#[test]
fn sha1_matches_known_digests() {
    let hex = |bytes: [u8; 20]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    assert_eq!(
        hex(crate::sha1::digest(b"abc")),
        "a9993e364706816aba3e25717850c26c9cd0d89d"
    );
    assert_eq!(
        hex(crate::sha1::digest(b"")),
        "da39a3ee5e6b4b0d3255bfef95601890afd80709"
    );
}
