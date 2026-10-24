//! 收件箱的传输：一行请求、一行回复（cross-session.md §2.1）。

use super::*;

#[tokio::test]
async fn a_request_gets_the_reply_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("1.sock");
    let listener = UdsListener::bind(&path).unwrap();
    assert_eq!(listener.path(), path.as_path());
    let server = tokio::spawn(async move {
        let mut stream = listener.accept().await.unwrap();
        let line = stream.read_line().await.unwrap().unwrap();
        stream.write_line(&format!("got {line}")).await.unwrap();
    });
    let reply = request(&path, "{\"type\":\"user\"}").await.unwrap();
    assert_eq!(reply.as_deref(), Some("got {\"type\":\"user\"}"));
    server.await.unwrap();
}

#[tokio::test]
async fn connecting_to_a_missing_socket_fails() {
    let dir = tempfile::tempdir().unwrap();
    let error = request(&dir.path().join("gone.sock"), "x")
        .await
        .unwrap_err();
    assert!(matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
    ));
}

/// 字节流的两半（常驻执行器的 socket，remote-exec.md §19.5）：原样往返，不按行切。
#[tokio::test]
async fn raw_halves_carry_bytes_both_ways() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("raw.sock");
    let listener = UdsListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let mut halves = listener.accept_raw().await.unwrap();
        let mut buffer = [0u8; 3];
        halves.read.read_exact(&mut buffer).await.unwrap();
        halves
            .write
            .write_all(&[buffer[2], buffer[1], buffer[0]])
            .await
            .unwrap();
        halves.write.shutdown().await.unwrap();
    });
    let mut halves = connect_raw(&path).await.unwrap();
    halves.write.write_all(&[1, 0, 2]).await.unwrap();
    let mut back = Vec::new();
    halves.read.read_to_end(&mut back).await.unwrap();
    assert_eq!(back, [2, 0, 1]);
    server.await.unwrap();
}
