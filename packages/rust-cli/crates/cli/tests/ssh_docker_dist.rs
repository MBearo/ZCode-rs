//! 正式版的部署来源（F100，docs/specs/rust-cli/remote-exec.md §26）：本机有 Docker 时，真实的 `zcode -p` 经机器清单连一台
//! sshd 容器，没有给上传用的产物，执行器从本地的假 npm 注册表下载：
//!
//! 1. debian（glibc）：下载 → integrity / build.json / ELF 校验 → 原子写进本机缓存 → 上传（试跑）→ 握手 → `Bash{machine}`
//!    在容器里跑；再连一次（远端的执行器删掉）用缓存，不再下载；
//! 2. alpine（musl）：下载之前报清楚的错误，注册表一次请求都没收到。
//!
//! 包是把 `test:rust-cli` 交叉编译的 musl 静态 debug 产物（`ZCODE_TEST_SSH_ARTIFACT`）照 npm 平台包的样子打成
//! `@mbearo/zcode-linux-<arch>-gnu@<debug 版本>`（静态产物在 glibc 的系统上照样能跑）。没有 Docker 或没有产物时跳过并
//! 写明原因。容器与镜像用完删掉（名字带 f100 前缀）。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    clippy::print_stderr,
    clippy::unwrap_used,
    reason = "集成测试：起 Docker 容器、本地 HTTP 服务与 zcode 进程；跳过时写明原因"
)]

mod sse;
mod sshd;

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};

use base64::Engine;
use serde_json::Value;
use sha2::{Digest, Sha256, Sha512};
use sshd::{BIN, HOST, Image, Sshd, VERSION, artifact_or_skip, run};

type Routes = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

/// 本地的假注册表：按路径回复字节（没有的路径 404），记下请求的路径。
struct Registry {
    url: String,
    routes: Routes,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Registry {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let routes: Routes = Arc::default();
        let (log, table) = (seen.clone(), routes.clone());
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
                    .find(|(route, _)| *route == path)
                    .map_or((404, b"{}".to_vec()), |(_, body)| (200, body.clone()));
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

    fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// 产物的架构（ELF 头的 `e_machine`）→（包 id，gnu target）。
fn platform(artifact: &Path) -> (&'static str, &'static str) {
    let bytes = std::fs::read(artifact).unwrap();
    match u16::from_le_bytes([bytes[18], bytes[19]]) {
        183 => ("linux-arm64-gnu", "aarch64-unknown-linux-gnu"),
        62 => ("linux-x64-gnu", "x86_64-unknown-linux-gnu"),
        other => panic!("unexpected ELF machine {other}"),
    }
}

/// 照 npm 平台包的样子打包（`package/bin/zcode-rs` 0755、`package/build.json`），返回 tarball 的字节。
fn pack(artifact: &Path, target: &str, dir: &Path) -> Vec<u8> {
    let package = dir.join("package");
    std::fs::create_dir_all(package.join("bin")).unwrap();
    let binary = package.join("bin/zcode-rs");
    std::fs::copy(artifact, &binary).unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    let bytes = std::fs::read(&binary).unwrap();
    let build = serde_json::json!({
        "version": VERSION,
        "sourceCommit": "0".repeat(40),
        "target": target,
        "sha256": hex(&Sha256::digest(&bytes)),
    });
    std::fs::write(package.join("build.json"), build.to_string()).unwrap();
    std::fs::write(package.join("package.json"), "{}").unwrap();
    let tgz = dir.join("package.tgz");
    let output = Command::new("tar")
        .args(["--format", "ustar", "-czf"])
        .arg(&tgz)
        .arg("-C")
        .arg(dir)
        .arg("package")
        .env("COPYFILE_DISABLE", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "tar: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::read(tgz).unwrap()
}

/// 起一个假注册表并发布这个包（packument 与 tarball 两条路由）。返回（注册表，packument 的路径，tarball 的路径）。
fn registry_for(id: &str, tarball: Vec<u8>) -> (Registry, String, String) {
    let registry = Registry::start();
    let name = format!("@mbearo/zcode-{id}");
    let packument = format!("/@mbearo%2fzcode-{id}");
    let tarball_path = format!("/{name}/-/zcode-{id}-{VERSION}.tgz");
    let integrity = format!(
        "sha512-{}",
        base64::engine::general_purpose::STANDARD.encode(Sha512::digest(&tarball))
    );
    let document = serde_json::json!({
        "name": name,
        "dist-tags": {"latest": VERSION},
        "versions": {VERSION: {"name": name, "version": VERSION, "dist": {
            "tarball": format!("{}{tarball_path}", registry.url),
            "integrity": integrity,
        }}},
    });
    registry.routes.lock().unwrap().extend([
        (packument.clone(), document.to_string().into_bytes()),
        (tarball_path.clone(), tarball),
    ]);
    (registry, packument, tarball_path)
}

/// 机器清单：dev-a 经 ssh 连 `HOST`，工作目录 `/srv/proj`。
fn machine_list(home: &Path) {
    let list = home.join(".zcode/rust-cli/machines.json");
    std::fs::create_dir_all(list.parent().unwrap()).unwrap();
    std::fs::write(
        &list,
        serde_json::json!({"version": 1, "machines": [
            {"name": "dev-a", "ssh": HOST, "workingDirectory": "/srv/proj", "connected": true}
        ]})
        .to_string(),
    )
    .unwrap();
}

/// 跑一次 `zcode -p`：脚本模型调用 `Bash{machine: dev-a}`。没有给上传用的产物，只给假注册表。
fn print_run(sshd: &Sshd, home: &Path, work: &Path, registry: &str) -> (Output, Vec<Value>) {
    let bash = serde_json::json!({
        "command": "uname -s; cat /etc/debian_version; echo marker-$((6*7))",
        "description": "Where am I",
        "machine": "dev-a"
    })
    .to_string();
    let fixture = sse::serve(vec![
        sse::tools_reply(&[sse::tool_block(0, "toolu_1", "Bash", &bash)]),
        sse::text_reply("done"),
    ]);
    let output = Command::new(BIN)
        .args([
            "-p",
            "check dev-a",
            "--permission-mode",
            "bypassPermissions",
        ])
        .current_dir(work)
        .env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .env("SHELL", "/bin/bash")
        .env("ZCODE_DATA_BASE_DIR", home)
        .env("ZCODE_MODEL_BASE_URL", &fixture.url)
        .env("ZCODE_API_KEY", "test-key")
        .env("ZCODE_MODEL", "glm-5")
        .env("ZCODE_RUNTIME_ENV", "test")
        .env("_ZCODE_ASSUME_SSH_MACHINES", "1")
        .env("_ZCODE_ASSUME_SSH_CONFIG", sshd.config())
        .env("_ZCODE_ASSUME_NPM_REGISTRY", registry)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    (output, fixture.main_requests())
}

fn last_result(requests: &[Value]) -> String {
    requests
        .get(1)
        .and_then(|request| request["messages"].as_array())
        .and_then(|messages| messages.last())
        .map(Value::to_string)
        .unwrap_or_default()
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[tokio::test(flavor = "multi_thread")]
async fn a_glibc_machine_downloads_the_executor_from_the_registry() {
    let Some(artifact) = artifact_or_skip() else {
        return;
    };
    let (id, target) = platform(&artifact);
    let packing = tempfile::tempdir().unwrap();
    let tarball = pack(&artifact, target, packing.path());
    let (registry, packument, tarball_path) = registry_for(id, tarball);
    let sshd = Sshd::start_with(Image::Debian, "f100-zcode-rs-test-sshd");
    sshd.wait_ready().await;
    let (ok, _, err) = sshd.ssh("mkdir -p /srv/proj && ldd --version | head -n 1");
    assert!(ok, "{err}");
    let work = tempfile::tempdir().unwrap();
    let work_path = std::fs::canonicalize(work.path()).unwrap();
    let home = tempfile::tempdir().unwrap();
    machine_list(home.path());

    // 1. 第一次：下载、校验、缓存、上传、握手，Bash 在容器里跑。
    let (output, requests) = print_run(&sshd, home.path(), &work_path, &registry.url);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result = last_result(&requests);
    assert!(result.contains("Linux"), "{result}");
    assert!(result.contains("marker-42"), "{result}");
    assert!(result.contains("12."), "debian_version: {result}");
    assert_eq!(registry.seen(), [packument, tarball_path]);
    let cached: PathBuf = home.path().join(format!(
        ".zcode/rust-cli/exec/dist/{VERSION}/{target}/zcode-rs"
    ));
    assert_eq!(
        std::fs::read(&cached).unwrap(),
        std::fs::read(&artifact).unwrap()
    );
    assert_eq!(mode(&cached), 0o755);
    assert_eq!(mode(cached.parent().unwrap()), 0o700);
    let installed = format!("/root/.zcode/rust-cli/exec/bin/{VERSION}/zcode-rs");
    let (_, remote_sum, _) = sshd.ssh(&format!("sha256sum {installed}"));
    let (_, local_sum, _) = run("shasum", &["-a", "256", &artifact.to_string_lossy()]);
    assert_eq!(
        remote_sum.split_whitespace().next(),
        local_sum.split_whitespace().next()
    );

    // 2. 远端的执行器停掉、删掉，再连一次：用本机缓存重新上传，不再下载。
    let (ok, out, err) = sshd.ssh(&format!(
        "{installed} executor stop && rm -rf /root/.zcode/rust-cli/exec/bin"
    ));
    assert!(ok, "{out}{err}");
    let (output, requests) = print_run(&sshd, home.path(), &work_path, &registry.url);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(last_result(&requests).contains("marker-42"));
    assert_eq!(
        registry.seen().len(),
        2,
        "no second download: {:?}",
        registry.seen()
    );
    let (ok, _, _) = sshd.ssh(&format!("test -x {installed}"));
    assert!(ok, "uploaded again from the cache");

    // 收尾：停掉容器里的常驻执行器（容器随后删掉）。
    let (ok, out, err) = sshd.ssh(&format!("{installed} executor stop"));
    assert!(ok, "{out}{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_musl_machine_is_refused_before_downloading() {
    let Some(artifact) = artifact_or_skip() else {
        return;
    };
    let (id, target) = platform(&artifact);
    let packing = tempfile::tempdir().unwrap();
    let tarball = pack(&artifact, target, packing.path());
    let (registry, _, _) = registry_for(id, tarball);
    let sshd = Sshd::start_with(Image::Alpine, "f100-zcode-rs-test-sshd");
    sshd.wait_ready().await;
    let (ok, _, err) = sshd.ssh("mkdir -p /srv/proj");
    assert!(ok, "{err}");
    let work = tempfile::tempdir().unwrap();
    let work_path = std::fs::canonicalize(work.path()).unwrap();
    let home = tempfile::tempdir().unwrap();
    machine_list(home.path());
    let (output, requests) = print_run(&sshd, home.path(), &work_path, &registry.url);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("the remote uses musl libc; there is no zcode executor package for it"),
        "{stderr}"
    );
    assert!(
        requests.is_empty(),
        "no model request before the machine connects"
    );
    assert!(registry.seen().is_empty(), "{:?}", registry.seen());
    assert!(!home.path().join(".zcode/rust-cli/exec/dist").exists());
}
