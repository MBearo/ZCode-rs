//! WebFetch 出口：经本地 HTTP 夹具核对一次 GET 的状态、header 与正文，重定向不跟随，正文超限报错。

use rust_cli_kernel_api::env_web::FetchUrl;
use rust_cli_net::{Egress, NetworkPolicy};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::*;

/// 只回答一个请求的本地服务；返回 URL 与收到的请求头。
#[expect(clippy::disallowed_methods, reason = "测试搭本地 HTTP 夹具")]
async fn serve(reply: &'static str) -> (String, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://127.0.0.1:{}/page",
        listener.local_addr().unwrap().port()
    );
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut seen = Vec::new();
        let mut buffer = [0u8; 4096];
        while !String::from_utf8_lossy(&seen).contains("\r\n\r\n") {
            let read = socket.read(&mut buffer).await.unwrap();
            if read == 0 {
                break;
            }
            seen.extend_from_slice(&buffer[..read]);
        }
        socket.write_all(reply.as_bytes()).await.unwrap();
        String::from_utf8_lossy(&seen).into_owned()
    });
    (url, task)
}

async fn egress() -> Egress {
    Egress::new(NetworkPolicy::default()).await.unwrap()
}

fn request(url: String, max_bytes: u64) -> FetchUrl {
    FetchUrl {
        url,
        headers: vec![(
            "Accept".to_owned(),
            "text/markdown, text/html, */*".to_owned(),
        )],
        timeout_ms: 5_000,
        max_bytes,
    }
}

#[tokio::test]
async fn a_page_comes_back_with_its_content_type() {
    let (url, seen) = serve(
        "HTTP/1.1 200 OK\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: 11\r\nconnection: close\r\n\r\n<p>hello</p",
    )
    .await;
    let page = fetch(&egress().await, request(url, 1024)).await.unwrap();
    assert_eq!(page.status, 200);
    assert_eq!(page.content_type, "text/html; charset=utf-8");
    assert_eq!(page.body, b"<p>hello</p");
    let head = seen.await.unwrap().to_ascii_lowercase();
    assert!(head.starts_with("get /page"));
    assert!(head.contains("accept: text/markdown, text/html, */*"));
}

#[tokio::test]
async fn redirects_are_returned_not_followed() {
    let (url, _seen) = serve(
        "HTTP/1.1 301 Moved Permanently\r\nlocation: https://example.com/new\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
    )
    .await;
    let page = fetch(&egress().await, request(url, 1024)).await.unwrap();
    assert_eq!(page.status, 301);
    assert_eq!(page.location.as_deref(), Some("https://example.com/new"));
}

#[tokio::test]
async fn bodies_over_the_limit_fail() {
    let (url, _seen) = serve(
        "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 10\r\nconnection: close\r\n\r\n0123456789",
    )
    .await;
    let error = fetch(&egress().await, request(url, 4)).await.unwrap_err();
    assert_eq!(
        error,
        EnvError::Io {
            message: "maxContentLength size of 4 exceeded".to_owned()
        }
    );
}
