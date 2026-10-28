//! 本地假 npm 注册表与平台包的夹具（F100，docs/specs/rust-cli/remote-exec.md §26.4）：按路径回复字节、记下请求的路径；
//! 在内存里打 npm 样子的 tarball（gzip + ustar，每项在 `package/` 下）。不连任何真实服务。

#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "测试直接搭本地 HTTP 夹具（docs/specs/rust-cli/README.md §4）"
)]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use base64::Engine;
use rust_cli_ssh_connect::Platform;
use sha2::{Digest, Sha256, Sha512};

type Routes = Arc<Mutex<Vec<(String, u16, Vec<u8>)>>>;

pub(crate) struct FakeNpm {
    pub(crate) url: String,
    routes: Routes,
    seen: Arc<Mutex<Vec<String>>>,
}

impl FakeNpm {
    /// 起服务（还没有路由：没有的路径回 404）。每个连接一个请求。
    pub(crate) fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let routes: Routes = Arc::default();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (table, log) = (routes.clone(), seen.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                if reader.read_line(&mut first).unwrap_or(0) == 0 {
                    continue;
                }
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                }
                let path = first.split_whitespace().nth(1).unwrap_or("").to_owned();
                log.lock().unwrap().push(path.clone());
                let (status, body) = table
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|(route, _, _)| *route == path)
                    .map_or((404, b"{}".to_vec()), |(_, status, body)| {
                        (*status, body.clone())
                    });
                let head = format!(
                    "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        Self { url, routes, seen }
    }

    pub(crate) fn route(&self, path: &str, status: u16, body: Vec<u8>) {
        self.routes
            .lock()
            .unwrap()
            .push((path.to_owned(), status, body));
    }

    pub(crate) fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }

    /// 发布一个包：packument（`integrity` 缺省是 tarball 的 sha512）与 tarball。返回 packument 的路径。
    pub(crate) fn publish(
        &self,
        package: &str,
        version: &str,
        tarball: &[u8],
        integrity: Option<String>,
    ) -> String {
        let short = package.rsplit('/').next().unwrap();
        let tarball_path = format!("/{package}/-/{short}-{version}.tgz");
        let document = serde_json::json!({
            "name": package,
            "dist-tags": {"latest": version},
            "versions": {version: {
                "name": package,
                "version": version,
                "dist": {
                    "tarball": format!("{}{tarball_path}", self.url),
                    "integrity": integrity.unwrap_or_else(|| sri(tarball)),
                },
            }},
        });
        let packument = format!("/{}", package.replace('/', "%2f"));
        self.route(&packument, 200, document.to_string().into_bytes());
        self.route(&tarball_path, 200, tarball.to_vec());
        packument
    }
}

/// `sha512-<base64>`。
pub(crate) fn sri(bytes: &[u8]) -> String {
    format!(
        "sha512-{}",
        base64::engine::general_purpose::STANDARD.encode(Sha512::digest(bytes))
    )
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// 一个这个平台的产物：Linux 是 ELF 头，macOS 是 Mach-O 头（F107，remote-exec.md §28.4），后面加一些内容。
pub(crate) fn image(platform: Platform) -> Vec<u8> {
    let mut bytes = vec![0u8; 64];
    match platform {
        Platform::LinuxX86_64 | Platform::LinuxAarch64 => {
            bytes[..6].copy_from_slice(b"\x7fELF\x02\x01");
            bytes[18] = if platform == Platform::LinuxX86_64 {
                62
            } else {
                183
            };
        }
        Platform::MacosAarch64 | Platform::MacosX86_64 => {
            bytes[..4].copy_from_slice(&0xfeed_facfu32.to_le_bytes());
            let cpu: u32 = if platform == Platform::MacosAarch64 {
                0x0100_000c
            } else {
                0x0100_0007
            };
            bytes[4..8].copy_from_slice(&cpu.to_le_bytes());
        }
    }
    bytes.extend_from_slice(&[7u8; 3000]);
    bytes
}

pub(crate) fn build_json(version: &str, target: &str, sha256: &str) -> Vec<u8> {
    serde_json::json!({
        "version": version,
        "sourceCommit": "0".repeat(40),
        "target": target,
        "sha256": sha256,
    })
    .to_string()
    .into_bytes()
}

/// 一个 tar 项的头（ustar；`prefix` 放进 345..500）。
pub(crate) fn header(name: &str, prefix: &str, size: usize, kind: u8) -> [u8; 512] {
    let mut header = [0u8; 512];
    header[..name.len()].copy_from_slice(name.as_bytes());
    header[100..108].copy_from_slice(b"0000755\0");
    header[108..116].copy_from_slice(b"0000000\0");
    header[116..124].copy_from_slice(b"0000000\0");
    header[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
    header[136..148].copy_from_slice(b"00000000000\0");
    header[156] = kind;
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    header[345..345 + prefix.len()].copy_from_slice(prefix.as_bytes());
    header[148..156].copy_from_slice(b"        ");
    let sum: u32 = header.iter().map(|byte| u32::from(*byte)).sum();
    header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    header
}

/// 原样的 tar（不压缩）：每项（头，内容）。
pub(crate) fn tar(entries: &[([u8; 512], Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (header, data) in entries {
        out.extend_from_slice(header);
        out.extend_from_slice(data);
        out.resize(out.len().div_ceil(512) * 512, 0);
    }
    out.extend_from_slice(&[0u8; 1024]);
    out
}

pub(crate) fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

/// npm 样子的平台包：`package/package.json`、`package/build.json`、`package/bin/zcode-rs`。
pub(crate) fn package(binary: &[u8], build: &[u8]) -> Vec<u8> {
    let manifest = br#"{"name":"@mbearo/zcode-linux-x64-gnu"}"#.to_vec();
    gzip(&tar(&[
        (
            header("package/package.json", "", manifest.len(), b'0'),
            manifest,
        ),
        (
            header("package/build.json", "", build.len(), b'0'),
            build.to_vec(),
        ),
        (
            header("package/bin/zcode-rs", "", binary.len(), b'0'),
            binary.to_vec(),
        ),
    ]))
}
