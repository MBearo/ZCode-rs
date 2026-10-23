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
