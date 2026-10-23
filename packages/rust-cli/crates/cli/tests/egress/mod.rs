//! 网络出口真实进程测试的夹具（docs/specs/rust-cli/config.md §5，修复线 F21）：现生成的 CA 与服务器证书、
//! 本地 HTTPS 模型端点（rustls 服务端，回一段 SSE 文本）、记录 CONNECT 目标的 HTTP 代理。都只监听 127.0.0.1，
//! 不依赖外网。
#![allow(dead_code, reason = "不同的测试用到的夹具不同")]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, DnType, ExtendedKeyUsagePurpose, IsCa,
    KeyPair, KeyUsagePurpose,
};
use rust_cli_kernel_api::clock::civil_date;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

/// 一套证书：CA 的 PEM（交给 `ZCODE_AGENT_CA_CERT` / `NODE_EXTRA_CA_CERTS`），以及它签的服务器证书与私钥
///（`localhost`、`127.0.0.1`）。
pub struct Pki {
    pub ca_pem: String,
    leaf: CertificateDer<'static>,
    key: Vec<u8>,
}

/// 有效期：昨天到 30 天后（UTC 日期）。macOS 的系统校验（`NODE_EXTRA_CA_CERTS` 走平台校验器）拒绝超过 825 天的
/// 服务器证书，所以不能用 rcgen 缺省的几千年。
fn validity(params: &mut CertificateParams) {
    let now = u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let date = |ms: u64| {
        let text = civil_date(ms);
        let parts: Vec<&str> = text.split('-').collect();
        rcgen::date_time_ymd(
            parts[0].parse().unwrap(),
            parts[1].parse().unwrap(),
            parts[2].parse().unwrap(),
        )
    };
    let day = 86_400_000;
    params.not_before = date(now - day);
    params.not_after = date(now + 30 * day);
}

/// 现生成一套证书。`name` 只用在 CA 的名字里，区分两套。
pub fn pki(name: &str) -> Pki {
    let mut ca = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca.distinguished_name
        .push(DnType::CommonName, format!("{name} test CA"));
    ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    validity(&mut ca);
    let ca = CertifiedIssuer::self_signed(ca, KeyPair::generate().unwrap()).unwrap();

    let mut leaf =
        CertificateParams::new(vec!["localhost".to_owned(), "127.0.0.1".to_owned()]).unwrap();
    leaf.distinguished_name
        .push(DnType::CommonName, "localhost");
    leaf.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    leaf.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    validity(&mut leaf);
    let key = KeyPair::generate().unwrap();
    let certificate = leaf.signed_by(&key, &ca).unwrap();
    Pki {
        ca_pem: ca.pem(),
        leaf: certificate.der().clone(),
        key: key.serialize_der(),
    }
}

/// 本地 HTTPS 模型端点：每个连接一个请求，回同一段 SSE 文本；记下请求行。
pub struct TlsModel {
    pub port: u16,
    requests: Arc<Mutex<Vec<String>>>,
}

impl TlsModel {
    pub fn url(&self) -> String {
        format!("https://127.0.0.1:{}", self.port)
    }

    /// 收到的请求行（`POST /v1/messages?beta=true`）。握手失败的连接不算。
    pub fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

pub fn tls_model(pki: &Pki, reply: String) -> TlsModel {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = Arc::new(
        rustls::ServerConfig::builder_with_provider(provider)
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
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = requests.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let config = config.clone();
            let recorded = recorded.clone();
            let reply = reply.clone();
            std::thread::spawn(move || {
                let connection = rustls::ServerConnection::new(config).unwrap();
                let mut tls = rustls::StreamOwned::new(connection, stream);
                // 证书不受信任时客户端在握手里断开：读不出请求，什么都不记。
                let Some((line, _)) = read_request(&mut tls) else {
                    return;
                };
                recorded.lock().unwrap().push(line);
                let head = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    reply.len()
                );
                let _ = tls.write_all(head.as_bytes());
                let _ = tls.write_all(reply.as_bytes());
                let _ = tls.flush();
                tls.conn.send_close_notify();
                let _ = tls.flush();
            });
        }
    });
    TlsModel { port, requests }
}

/// 读一个 HTTP/1.1 请求（请求头逐字节读，不多读）：返回请求行与请求体。
fn read_request(stream: &mut impl Read) -> Option<(String, Vec<u8>)> {
    let head = read_head(stream)?;
    let line = head.lines().next()?.to_owned();
    let length = head
        .lines()
        .find_map(|line| {
            let lower = line.to_ascii_lowercase();
            lower
                .strip_prefix("content-length:")
                .map(|value| value.trim().parse::<usize>().unwrap_or(0))
        })
        .unwrap_or(0);
    let mut body = vec![0; length];
    stream.read_exact(&mut body).ok()?;
    Some((line, body))
}

/// 读到空行为止的请求头。一次读一个字节：CONNECT 之后的隧道字节不能被缓冲吃掉。
fn read_head(stream: &mut impl Read) -> Option<String> {
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).ok()? == 0 {
            return None;
        }
        head.push(byte[0]);
    }
    String::from_utf8(head).ok()
}

/// 记录 CONNECT 目标的 HTTP 代理：建隧道到目标（只会是 127.0.0.1 上的夹具），双向转发字节。其他方法回 502
/// 并照样记下（`POST http://…`）。
pub struct ConnectProxy {
    pub port: u16,
    targets: Arc<Mutex<Vec<String>>>,
}

impl ConnectProxy {
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// 收到的目标：CONNECT 的 `主机:端口`，其他方法是 `方法 目标`。
    pub fn targets(&self) -> Vec<String> {
        self.targets.lock().unwrap().clone()
    }
}

pub fn connect_proxy() -> ConnectProxy {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let targets = Arc::new(Mutex::new(Vec::new()));
    let recorded = targets.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut client) = stream else { continue };
            let recorded = recorded.clone();
            std::thread::spawn(move || {
                let Some(head) = read_head(&mut client) else {
                    return;
                };
                let mut words = head.lines().next().unwrap_or("").split(' ');
                let method = words.next().unwrap_or("").to_owned();
                let target = words.next().unwrap_or("").to_owned();
                if method != "CONNECT" {
                    recorded.lock().unwrap().push(format!("{method} {target}"));
                    let _ = client.write_all(
                        b"HTTP/1.1 502 Bad Gateway\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                    );
                    return;
                }
                recorded.lock().unwrap().push(target.clone());
                let Ok(upstream) = TcpStream::connect(&target) else {
                    let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n");
                    return;
                };
                let _ = client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n");
                tunnel(client, upstream);
            });
        }
    });
    ConnectProxy { port, targets }
}

/// 双向转发，任一方向读完就关掉两边。
fn tunnel(client: TcpStream, upstream: TcpStream) {
    let (mut client_read, mut upstream_write) =
        (client.try_clone().unwrap(), upstream.try_clone().unwrap());
    let forward = std::thread::spawn(move || {
        let _ = std::io::copy(&mut client_read, &mut upstream_write);
        let _ = upstream_write.shutdown(std::net::Shutdown::Write);
    });
    let (mut upstream_read, mut client_write) = (upstream, client);
    let _ = std::io::copy(&mut upstream_read, &mut client_write);
    let _ = client_write.shutdown(std::net::Shutdown::Both);
    let _ = forward.join();
}
