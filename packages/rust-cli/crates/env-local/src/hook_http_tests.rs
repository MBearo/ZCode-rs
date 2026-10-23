//! http hook 出口：经本地 HTTP 夹具核对请求与响应、超时与取消。

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::*;

/// 只回答一个请求的本地服务；返回 URL 与收到的原始请求。
#[expect(clippy::disallowed_methods, reason = "测试搭本地 HTTP 夹具")]
async fn serve(reply: &'static str, hang: bool) -> (String, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://127.0.0.1:{}/hook",
        listener.local_addr().unwrap().port()
    );
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut seen = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            let read = socket.read(&mut buffer).await.unwrap();
            seen.extend_from_slice(&buffer[..read]);
            let text = String::from_utf8_lossy(&seen).into_owned();
            if let Some((head, body)) = text.split_once("\r\n\r\n") {
                let length = head
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .map(str::to_owned)
                    })
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if body.len() >= length || read == 0 {
                    break;
                }
            }
        }
        if hang {
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
        socket.write_all(reply.as_bytes()).await.unwrap();
        String::from_utf8_lossy(&seen).into_owned()
    });
    (url, task)
}

async fn egress() -> Arc<Egress> {
    Arc::new(
        Egress::new(rust_cli_net::NetworkPolicy::default())
            .await
            .unwrap(),
    )
}

fn request(url: String, timeout_ms: u64) -> HookHttpRequest {
    HookHttpRequest {
        url,
        headers: vec![
            ("Content-Type".to_owned(), "application/json".to_owned()),
            ("X-Hook".to_owned(), "yes".to_owned()),
        ],
        body: "{\"hook_event_name\":\"Stop\"}".to_owned(),
        timeout_ms,
    }
}

#[tokio::test]
async fn posts_the_input_and_reads_the_body() {
    let (url, server) = serve(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nconnection: close\r\ncontent-length: 13\r\n\r\n{\"ok\": true }",
        false,
    )
    .await;
    let port = LocalHookHttp::new(egress().await);
    let response = port
        .post(request(url, 5_000), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        response,
        HookHttpResponse {
            status: 200,
            body: "{\"ok\": true }".to_owned()
        }
    );
    let seen = server.await.unwrap();
    assert!(seen.starts_with("POST /hook HTTP/1.1\r\n"));
    let lower = seen.to_ascii_lowercase();
    assert!(lower.contains("content-type: application/json\r\n"));
    assert!(lower.contains("x-hook: yes\r\n"));
    assert!(seen.ends_with("{\"hook_event_name\":\"Stop\"}"));
}

#[tokio::test]
async fn times_out_and_cancels() {
    let (url, _server) = serve("", true).await;
    let port = LocalHookHttp::new(egress().await);
    assert_eq!(
        port.post(request(url.clone(), 50), CancellationToken::new())
            .await,
        Err(HookHttpError::TimedOut)
    );
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        port.post(request(url, 60_000), cancel).await,
        Err(HookHttpError::Cancelled)
    );
    let refused = port
        .post(
            request("http://127.0.0.1:9/none".to_owned(), 5_000),
            CancellationToken::new(),
        )
        .await;
    assert!(matches!(refused, Err(HookHttpError::Failed { .. })));
}

/// 缺陷 R2-13：配置给的 header（插值后常带 token）原来按普通值交给 net；现在是秘密值，Debug 里看不到。
#[test]
fn configured_headers_are_secret() {
    let mut hook = request("http://127.0.0.1:9/hook".to_owned(), 1_000);
    hook.headers
        .push(("Authorization".to_owned(), "Bearer t0k-secret".to_owned()));
    let shown = format!("{:?}", outbound(hook));
    assert!(shown.contains("Authorization"), "{shown}");
    assert!(!shown.contains("t0k-secret"), "{shown}");
}
