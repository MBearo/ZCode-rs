//! 根证书的合并在 TLS 握手里的效果（docs/specs/rust-cli/config.md §2.4，工作线 F44）。本地 HTTPS 服务用测试里现生成
//! 的 CA 与服务器证书，只监听 127.0.0.1。
//!
//! “系统根证书为空”（没有 ca-certificates 的 Linux 镜像）在 macOS 上做不出来：那里的平台校验器总有系统证书库。
//! 这里用 `build_client(…, false)` 模拟——不要平台根证书，只留交给它的附加信任锚。Linux 上平台校验器在系统证书为空
//! 时的信任集正是这些附加锚（rustls-platform-verifier `others.rs`：附加锚 + 系统 CA 文件里的证书）。
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;

use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, DnType, ExtendedKeyUsagePurpose, IsCa,
    KeyPair, KeyUsagePurpose,
};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};

use super::*;
use crate::CaBundle;

/// 现生成的 CA（PEM）与它签的 `127.0.0.1` 服务器证书。
struct Pki {
    ca_pem: String,
    leaf: rustls::pki_types::CertificateDer<'static>,
    key: Vec<u8>,
}

fn pki(name: &str) -> Pki {
    let mut ca = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca.distinguished_name
        .push(DnType::CommonName, format!("{name} test CA"));
    ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca = CertifiedIssuer::self_signed(ca, KeyPair::generate().unwrap()).unwrap();
    let mut leaf = CertificateParams::new(vec!["127.0.0.1".to_owned()]).unwrap();
    leaf.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let key = KeyPair::generate().unwrap();
    let certificate = leaf.signed_by(&key, &ca).unwrap();
    Pki {
        ca_pem: ca.pem(),
        leaf: certificate.der().clone(),
        key: key.serialize_der(),
    }
}

/// 本地 HTTPS 服务：每个连接回 `200 ok`。返回 URL。
#[allow(clippy::disallowed_methods, reason = "测试夹具在 127.0.0.1 上监听")]
fn serve(pki: &Pki) -> String {
    let config = Arc::new(
        rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![pki.leaf.clone()],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(pki.key.clone())),
        )
        .unwrap(),
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let config = config.clone();
            std::thread::spawn(move || {
                let connection = rustls::ServerConnection::new(config).unwrap();
                let mut tls = rustls::StreamOwned::new(connection, stream);
                // 读到请求头结束；证书不受信任时客户端在握手里断开，读不出来。
                let mut head = Vec::new();
                let mut byte = [0_u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    match tls.read(&mut byte) {
                        Ok(1) => head.push(byte[0]),
                        _ => return,
                    }
                }
                let _ = tls.write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok",
                );
                tls.conn.send_close_notify();
                let _ = tls.flush();
            });
        }
    });
    format!("https://127.0.0.1:{port}/")
}

fn policy(roots: TrustRoots) -> NetworkPolicy {
    NetworkPolicy {
        roots,
        ..NetworkPolicy::default()
    }
}

/// 发一个 GET：成功时是响应体，失败时是错误的调试文本。
#[allow(clippy::disallowed_methods, reason = "测试直接用构造好的客户端发请求")]
async fn get(client: &reqwest::Client, url: &str) -> Result<String, String> {
    match client.get(url).send().await {
        Ok(response) => Ok(response
            .text()
            .await
            .map_err(|error| format!("{error:?}"))?),
        Err(error) => Err(format!("{error:?}")),
    }
}

fn untrusted(outcome: Result<String, String>) -> bool {
    matches!(outcome, Err(text) if text.contains("UnknownIssuer"))
}

#[tokio::test]
async fn with_no_system_roots_the_bundled_roots_alone_still_build_a_client() {
    let server = pki("server");
    let url = serve(&server);
    // 只有内置的：客户端照常构造（以前 Linux 上这里就失败，`zcode` 启动即退出），本地 CA 不在内置列表里。
    let bundled = build_client(&policy(TrustRoots::Platform), false).unwrap();
    assert!(untrusted(get(&bundled, &url).await));
    // NODE_EXTRA_CA_CERTS：内置 + 文件里的。
    let extra = CaBundle::from_pem(server.ca_pem.as_bytes());
    let merged = build_client(&policy(TrustRoots::Extra(extra.clone())), false).unwrap();
    assert_eq!(get(&merged, &url).await, Ok("ok".to_owned()));
    // ZCODE_AGENT_CA_CERT：只信任文件里的。
    let only = build_client(&policy(TrustRoots::Only(extra)), false).unwrap();
    assert_eq!(get(&only, &url).await, Ok("ok".to_owned()));
    let other = CaBundle::from_pem(pki("other").ca_pem.as_bytes());
    let replaced = build_client(&policy(TrustRoots::Only(other)), false).unwrap();
    assert!(untrusted(get(&replaced, &url).await));
}

#[tokio::test]
async fn the_platform_verifier_takes_the_bundled_roots_as_extra_anchors() {
    let server = pki("server");
    let url = serve(&server);
    // 生产的客户端（平台校验器 + 内置的附加锚）照常构造；本地 CA 不受信任。
    let platform = client(&policy(TrustRoots::Platform)).unwrap();
    assert!(get(&platform, &url).await.is_err());
}
