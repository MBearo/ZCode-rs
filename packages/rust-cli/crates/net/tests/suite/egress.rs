use std::time::{Duration, Instant};

use rust_cli_base::secret::RedactedString;
use rust_cli_net::{
    CaBundle, Egress, HeaderValue, HttpRequest, NetError, NetworkPolicy, NoProxy, TrustRoots,
};

use super::fixture::{Fixture, Reply};

fn policy(headers: &[(&str, &str)]) -> NetworkPolicy {
    NetworkPolicy {
        headers: headers
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect(),
        ..NetworkPolicy::default()
    }
}

#[tokio::test]
async fn policy_headers_are_added_and_request_headers_win() {
    let server = Fixture::start(vec![Reply::new(200).send("ok")]).await;
    let egress = Egress::new(policy(&[("x-app", "cli"), ("user-agent", "zcode/test")]))
        .await
        .unwrap();
    let request = HttpRequest::post(format!("{}/v1/messages?beta=true", server.url()), "{}")
        .header("user-agent", "override")
        .header(
            "x-api-key",
            HeaderValue::Secret(RedactedString::new("sk-ant-secret-value")),
        );
    assert!(!format!("{request:?}").contains("sk-ant-secret-value"));
    let response = egress.send(request).await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.text(1024).await.unwrap(), "ok");
    let captured = &server.requests()[0];
    assert_eq!(captured.method, "POST");
    assert_eq!(captured.target, "/v1/messages?beta=true");
    assert_eq!(captured.header("x-app"), Some("cli"));
    assert_eq!(captured.header("user-agent"), Some("override"));
    assert_eq!(captured.header("x-api-key"), Some("sk-ant-secret-value"));
    assert_eq!(captured.body, "{}");
}

#[tokio::test]
async fn response_body_streams_chunk_by_chunk() {
    let reply = Reply::new(201)
        .header("retry-after", "3")
        .send("hello ")
        .pause(30)
        .send("world");
    let server = Fixture::start(vec![reply]).await;
    let egress = Egress::new(NetworkPolicy::default()).await.unwrap();
    let mut response = egress.send(HttpRequest::get(server.url())).await.unwrap();
    assert_eq!(response.status(), 201);
    assert!(response.is_success());
    assert_eq!(response.headers().get("Retry-After"), Some("3"));
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.unwrap() {
        body.extend_from_slice(&chunk);
    }
    assert_eq!(body, b"hello world");
}

#[tokio::test]
async fn text_reads_at_most_the_limit() {
    let server = Fixture::start(vec![Reply::new(500).send("0123456789")]).await;
    let egress = Egress::new(NetworkPolicy::default()).await.unwrap();
    let response = egress.send(HttpRequest::get(server.url())).await.unwrap();
    assert!(!response.is_success());
    assert_eq!(response.text(4).await.unwrap(), "0123");
}

#[tokio::test]
async fn a_replaced_policy_applies_to_the_next_request() {
    let server = Fixture::start(vec![Reply::new(200), Reply::new(200)]).await;
    let egress = Egress::new(policy(&[("x-policy", "v1")])).await.unwrap();
    egress.send(HttpRequest::get(server.url())).await.unwrap();
    egress.replace(policy(&[("x-policy", "v2")])).await.unwrap();
    egress.send(HttpRequest::get(server.url())).await.unwrap();
    let seen: Vec<_> = server
        .requests()
        .iter()
        .map(|request| request.header("x-policy").map(str::to_owned))
        .collect();
    assert_eq!(seen, vec![Some("v1".to_owned()), Some("v2".to_owned())]);

    // 无效的新策略不生效，旧策略继续使用。
    let invalid = NetworkPolicy {
        proxy: Some("::not a url::".to_owned()),
        ..policy(&[("x-policy", "v3")])
    };
    assert!(matches!(
        egress.replace(invalid).await,
        Err(NetError::Policy(_))
    ));
    assert_eq!(egress.policy().headers[0].1, "v2");
}

#[tokio::test]
async fn invalid_policies_are_rejected() {
    let bad_header = policy(&[("bad header", "x")]);
    assert!(matches!(
        Egress::new(bad_header).await,
        Err(NetError::Policy(_))
    ));
    // reqwest 遇到不认识的代理协议会静默直连，所以构造时拒绝（config.md D15）；错误不回显 URL（可能带密码）。
    for proxy in [
        "ftp://user:secret@proxy.example:21",
        "pac+http://proxy.example/wpad.dat",
    ] {
        let unsupported = NetworkPolicy {
            proxy: Some(proxy.to_owned()),
            ..NetworkPolicy::default()
        };
        let error = Egress::new(unsupported).await.err().unwrap();
        assert!(matches!(error, NetError::Policy(_)), "{error:?}");
        assert!(!error.to_string().contains("secret"), "{error}");
    }
    let no_host = NetworkPolicy {
        proxy: Some("socks5://".to_owned()),
        ..NetworkPolicy::default()
    };
    assert!(matches!(
        Egress::new(no_host).await,
        Err(NetError::Policy(_))
    ));
}

#[tokio::test]
async fn certificate_sets_that_read_as_empty_still_build_a_client() {
    // `ZCODE_AGENT_CA_CERT` 里没有可用证书：照常构造，HTTPS 在证书校验时失败（Node 的 `ca` 相同）。
    for roots in [
        TrustRoots::Only(CaBundle::from_pem(b"not a pem")),
        TrustRoots::Extra(CaBundle::from_pem(b"not a pem")),
    ] {
        let server = Fixture::start(vec![Reply::new(200).send("ok")]).await;
        let egress = Egress::new(NetworkPolicy {
            roots,
            ..NetworkPolicy::default()
        })
        .await
        .unwrap();
        // 明文 HTTP 不受根证书影响。
        let response = egress.send(HttpRequest::get(server.url())).await.unwrap();
        assert_eq!(response.text(16).await.unwrap(), "ok");
    }
}

#[tokio::test]
async fn a_no_proxy_match_skips_the_proxy() {
    let proxy = Fixture::start(vec![Reply::new(200).send("via proxy")]).await;
    let server = Fixture::start(vec![Reply::new(200).send("direct")]).await;
    for no_proxy in [
        NoProxy::Standard("example.test 127.0.0.1".to_owned()),
        NoProxy::Node("example.test,127.0.0.1".to_owned()),
    ] {
        let egress = Egress::new(NetworkPolicy {
            proxy: Some(proxy.url()),
            no_proxy,
            ..NetworkPolicy::default()
        })
        .await
        .unwrap();
        let server = Fixture::start(vec![Reply::new(200).send("direct")]).await;
        let response = egress.send(HttpRequest::get(server.url())).await.unwrap();
        assert_eq!(response.text(1024).await.unwrap(), "direct");
        assert_eq!(server.requests()[0].target, "/");
    }
    // 不命中时经代理（代理收到绝对形式的目标）。
    let egress = Egress::new(NetworkPolicy {
        proxy: Some(proxy.url()),
        no_proxy: NoProxy::Standard("example.test".to_owned()),
        ..NetworkPolicy::default()
    })
    .await
    .unwrap();
    let response = egress.send(HttpRequest::get(server.url())).await.unwrap();
    assert_eq!(response.text(1024).await.unwrap(), "via proxy");
    assert_eq!(proxy.requests()[0].target, format!("{}/", server.url()));
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn the_explicit_proxy_carries_the_request() {
    let proxy = Fixture::start(vec![Reply::new(200).send("via proxy")]).await;
    let egress = Egress::new(NetworkPolicy {
        proxy: Some(proxy.url()),
        ..NetworkPolicy::default()
    })
    .await
    .unwrap();
    let response = egress
        .send(HttpRequest::get("http://model.example.invalid/v1/models"))
        .await
        .unwrap();
    assert_eq!(response.text(1024).await.unwrap(), "via proxy");
    assert_eq!(
        proxy.requests()[0].target,
        "http://model.example.invalid/v1/models"
    );
}

#[tokio::test]
async fn waiting_for_headers_times_out() {
    let server = Fixture::start(vec![Reply::new(200).delay_head(5_000)]).await;
    let egress = Egress::new(NetworkPolicy::default()).await.unwrap();
    let mut request = HttpRequest::get(server.url());
    request.timeout = Some(Duration::from_millis(100));
    let started = Instant::now();
    let error = egress.send(request).await.unwrap_err();
    assert_eq!(error, NetError::Timeout);
    assert!(error.is_transient());
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[tokio::test]
async fn a_refused_connection_is_transient_and_redacted() {
    // 拿一个刚释放的端口：没有人监听，建连被拒绝。
    let port = {
        let server = Fixture::start(Vec::new()).await;
        server.url()
    };
    tokio::time::sleep(Duration::from_millis(20)).await;
    let egress = Egress::new(NetworkPolicy::default()).await.unwrap();
    let url = format!("{port}/v1/messages?api_key=sk-ant-0123456789abcdefghijklmn");
    let error = egress.send(HttpRequest::get(url)).await.unwrap_err();
    assert!(matches!(error, NetError::Connect(_)), "{error:?}");
    assert!(error.is_transient());
    assert!(!error.to_string().contains("sk-ant-0123456789"), "{error}");
}

#[tokio::test]
async fn an_invalid_url_is_a_request_error() {
    let egress = Egress::new(NetworkPolicy::default()).await.unwrap();
    let error = egress
        .send(HttpRequest::get("not a url"))
        .await
        .unwrap_err();
    assert!(matches!(error, NetError::Request(_)), "{error:?}");
    assert!(!error.is_transient());
}
