//! F107（docs/specs/rust-cli/remote-exec.md §28.10）：真实的 SSH 端到端，远端是 macOS——本机就是 macOS 时，经本测试自己
//! 起的 `sshd`（127.0.0.1 的高端口，临时密钥、临时 HOME，见 `sshd/local.rs`）走完整条链：
//!
//! 探测（`uname -s` = Darwin）→ 从本地的假 npm 注册表下载 `darwin-arm64` 的包 → 校验（integrity、build.json、Mach-O 头）
//! → 原子写进本机缓存 `dist/<版本>/aarch64-apple-darwin/` → 上传 → 改名之前试跑（签名要是无效就会被系统杀掉）→ 握手
//! → `Bash{machine: dev-a}` 在“远端”跑。第二次连接用本机缓存，不再下载。
//!
//! 包里的执行器就是本测试的二进制（本机的 macOS arm64 debug 产物）。不是 macOS、没有 `/usr/sbin/sshd` 或 sshd 起不来时
//! 跳过并写明原因；不改本机的任何系统设置。
#![cfg(target_os = "macos")]
#![allow(
    clippy::disallowed_methods,
    clippy::print_stderr,
    clippy::unwrap_used,
    reason = "集成测试：起 sshd、本地 HTTP 服务与 zcode 进程；跳过时写明原因"
)]

mod sse;
mod sshd;

use std::path::Path;
use std::process::{Command, Output, Stdio};

use serde_json::Value;
use sshd::local::{LocalSshd, Registry, pack, sshd_or_skip};
use sshd::{BIN, VERSION, run};

/// macOS arm64 的平台包与它的 target（§28.2）。
const ID: &str = "darwin-arm64";
const TARGET: &str = "aarch64-apple-darwin";

/// 机器清单：dev-a 经 ssh 连本测试的 sshd，工作目录是远端 HOME 下的一个目录。
fn machine_list(home: &Path, working: &Path) {
    let list = home.join(".zcode/rust-cli/machines.json");
    std::fs::create_dir_all(list.parent().unwrap()).unwrap();
    std::fs::write(
        &list,
        serde_json::json!({"version": 1, "machines": [
            {"name": "dev-a", "ssh": sshd::local::HOST,
             "workingDirectory": working.to_string_lossy(), "connected": true}
        ]})
        .to_string(),
    )
    .unwrap();
}

/// 跑一次 `zcode -p`：脚本模型调用 `Bash{machine: dev-a}`。没有给上传用的产物，只给假注册表。
fn print_run(sshd: &LocalSshd, home: &Path, work: &Path, registry: &str) -> (Output, Vec<Value>) {
    let bash = serde_json::json!({
        "command": "uname -s; uname -m; pwd; echo marker-$((6*7))",
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

#[tokio::test(flavor = "multi_thread")]
async fn a_macos_machine_downloads_uploads_and_runs_the_executor() {
    let Some(sshd) = sshd_or_skip() else {
        return;
    };
    // 远端是 macOS arm64（这台机器自己）。
    let (ok, facts, err) = sshd.ssh("uname -s; uname -m");
    assert!(ok, "{err}");
    let facts: Vec<&str> = facts.split_whitespace().collect();
    assert_eq!(facts.first().copied(), Some("Darwin"), "{facts:?}");
    if facts.get(1).copied() != Some("arm64") {
        eprintln!("macOS SSH test skipped: this machine is {facts:?}, not arm64");
        return;
    }

    // 本测试的二进制（macOS arm64 的 debug 产物）照平台包的样子打包，发布到本地的假注册表。
    let packing = tempfile::tempdir().unwrap();
    let tarball = pack(Path::new(BIN), TARGET, VERSION, packing.path());
    let registry = Registry::start();
    let (packument, tarball_path) = registry.publish(ID, VERSION, tarball);

    let remote_work = sshd.home().join("proj");
    std::fs::create_dir_all(&remote_work).unwrap();
    let work = tempfile::tempdir().unwrap();
    let work_path = std::fs::canonicalize(work.path()).unwrap();
    let home = tempfile::tempdir().unwrap();
    machine_list(home.path(), &remote_work);

    // 1. 第一次：下载、校验、缓存、上传、试跑、握手，Bash 在“远端”跑。
    let (output, requests) = print_run(&sshd, home.path(), &work_path, &registry.url);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result = last_result(&requests);
    assert!(result.contains("Darwin"), "{result}");
    assert!(result.contains("arm64"), "{result}");
    assert!(result.contains("marker-42"), "{result}");
    assert!(
        result.contains(&remote_work.to_string_lossy().into_owned()),
        "the remote working directory: {result}"
    );
    assert_eq!(registry.seen(), [packument, tarball_path]);

    // 缓存按 macOS 的 target 放（与 Linux 的 aarch64 不是同一个目录）。
    let dist = home.path().join(".zcode/rust-cli/exec/dist");
    let cached = dist.join(format!("{VERSION}/{TARGET}/zcode-rs"));
    let (_, local_sum, _) = run("shasum", &["-a", "256", BIN]);
    let (_, cached_sum, _) = run("shasum", &["-a", "256", &cached.to_string_lossy()]);
    assert_eq!(
        cached_sum.split_whitespace().next(),
        local_sum.split_whitespace().next()
    );
    assert!(
        !dist
            .join(format!("{VERSION}/aarch64-unknown-linux-gnu"))
            .exists()
    );

    // 上传到远端的执行器：sha256 相同、试跑没留东西、没有隔离属性（§28.5）。
    let installed = sshd
        .home()
        .join(format!(".zcode/rust-cli/exec/bin/{VERSION}/zcode-rs"));
    let installed = installed.to_string_lossy().into_owned();
    let (ok, remote_sum, err) = sshd.ssh(&format!("shasum -a 256 {installed}"));
    assert!(ok, "{err}");
    assert_eq!(
        remote_sum.split_whitespace().next(),
        local_sum.split_whitespace().next()
    );
    let (ok, listing, err) = sshd.ssh(&format!(
        "ls -A {home}/.zcode/rust-cli/exec/bin/{VERSION}",
        home = sshd.home().display()
    ));
    assert!(ok, "{err}");
    assert_eq!(
        listing.split_whitespace().collect::<Vec<_>>(),
        ["zcode-rs"],
        "no trial leftovers: {listing}"
    );
    let (ok, attributes, err) = sshd.ssh(&format!("xattr {installed}"));
    assert!(ok, "{err}");
    assert!(
        !attributes.contains("com.apple.quarantine"),
        "a file written over ssh is not quarantined: {attributes}"
    );
    // 常驻执行器的 socket 在远端的 HOME 下，路径不超过 macOS 的 104 字节上限（§28.7）。
    let socket = sshd.home().join(".zcode/rust-cli/exec/executor.sock");
    assert!(socket.to_string_lossy().len() < 104, "{}", socket.display());
    assert!(socket.exists(), "the resident executor is listening");

    // 2. 停掉远端的执行器、删掉 bin/，再连一次：用本机缓存重新上传，不再下载。
    let (ok, out, err) = sshd.ssh(&format!(
        "{installed} executor stop && rm -rf {home}/.zcode/rust-cli/exec/bin",
        home = sshd.home().display()
    ));
    assert!(ok, "{out}{err}");
    let before = registry.seen().len();
    let (output, requests) = print_run(&sshd, home.path(), &work_path, &registry.url);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(last_result(&requests).contains("marker-42"));
    assert_eq!(
        registry.seen().len(),
        before,
        "no second download: {:?}",
        registry.seen()
    );
    let (ok, _, _) = sshd.ssh(&format!("test -x {installed}"));
    assert!(ok, "uploaded again from the cache");

    // 收尾：停掉“远端”的常驻执行器（夹具随后杀掉 sshd、删掉临时目录）。
    let (ok, out, err) = sshd.ssh(&format!("{installed} executor stop"));
    assert!(ok, "{out}{err}");
}
