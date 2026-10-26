//! F100（remote-exec.md §26.2、§26.5）：部署器经注入的来源拿 glibc 的执行器——下载后上传并试跑、缓存命中不下载、下载
//! 失败退到协议相同的候选、musl 的远端不下载、glibc 太旧时写明远端的版本、本机的静态产物优先。远端是假的 `ssh`
//! （`provision_tests.rs` 的 `Remote`：glibc 2.31；目标 `alpine` 是 musl）。
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "测试直接准备文件系统夹具与进程"
)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_cli_exec_transport::{ProvisionError, ProvisionStep, Provisioner};

use crate::artifact::Arch;
use crate::provision_tests::{Remote, mode, wanted};
use crate::{ArtifactSource, SshProvisioner};

/// 假的来源：缓存里有没有、下载的结果；记下每次调用的架构。
struct FakeSource {
    cached: Option<PathBuf>,
    downloaded: Result<PathBuf, String>,
    cached_calls: AtomicUsize,
    downloads: Mutex<Vec<(String, Arch)>>,
}

impl FakeSource {
    fn new(cached: Option<PathBuf>, downloaded: Result<PathBuf, String>) -> Arc<Self> {
        Arc::new(Self {
            cached,
            downloaded,
            cached_calls: AtomicUsize::new(0),
            downloads: Mutex::new(Vec::new()),
        })
    }

    fn downloads(&self) -> Vec<(String, Arch)> {
        self.downloads.lock().unwrap().clone()
    }
}

#[async_trait]
impl ArtifactSource for FakeSource {
    async fn cached(&self, _version: &str, _arch: Arch) -> Option<PathBuf> {
        self.cached_calls.fetch_add(1, Ordering::SeqCst);
        self.cached.clone()
    }

    async fn download(&self, version: &str, arch: Arch) -> Result<PathBuf, String> {
        self.downloads
            .lock()
            .unwrap()
            .push((version.to_owned(), arch));
        self.downloaded.clone()
    }
}

/// 下载来的“执行器”：一个 shell 脚本（试跑的 `executor status --json` 按 `body` 回答）。
fn executor(remote: &Remote, name: &str, body: &str) -> PathBuf {
    let path = remote.root.path().join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// 试跑时说“常驻执行器没在跑”（退出码 3）。
const RUNS: &str = "[ \"$1 $2\" = 'executor status' ] && exit 3\nexit 9";

fn installed(remote: &Remote) -> PathBuf {
    remote
        .home()
        .join(".zcode/rust-cli/exec/bin/0.2.0/zcode-rs")
}

/// 远端已有一个协议相同的旧版本（例如 npm 装的）。
fn older_candidate(remote: &Remote) -> PathBuf {
    let older = remote
        .home()
        .join(".zcode/rust-cli/exec/bin/0.1.9/zcode-rs");
    std::fs::create_dir_all(older.parent().unwrap()).unwrap();
    std::fs::write(
        &older,
        "#!/bin/sh\necho '{\"version\":\"0.1.9\",\"protocol\":1,\"resident\":null}'\n",
    )
    .unwrap();
    std::fs::set_permissions(&older, std::fs::Permissions::from_mode(0o700)).unwrap();
    older
}

async fn provision(
    provisioner: &SshProvisioner,
) -> (Result<String, ProvisionError>, Vec<ProvisionStep>) {
    let seen = Mutex::new(Vec::new());
    let record = |step: ProvisionStep| seen.lock().unwrap().push(step);
    let result = provisioner
        .provision_reporting(&record)
        .await
        .map(|provisioned| provisioned.program);
    (result, seen.into_inner().unwrap())
}

#[tokio::test]
async fn a_downloaded_executor_is_uploaded_after_a_trial_run() {
    let remote = Remote::new();
    let file = executor(&remote, "downloaded", RUNS);
    let source = FakeSource::new(None, Ok(file.clone()));
    let provisioner = SshProvisioner::new(remote.target("dev-a"), wanted(), Vec::new())
        .with_source(source.clone());
    let (program, steps) = provision(&provisioner).await;
    assert_eq!(program.unwrap(), installed(&remote).to_string_lossy());
    let bytes = std::fs::read(&file).unwrap();
    assert_eq!(
        steps,
        [
            ProvisionStep::Downloading,
            ProvisionStep::Uploading {
                bytes: bytes.len() as u64
            }
        ]
    );
    assert_eq!(source.downloads(), [("0.2.0".to_owned(), Arch::X86_64)]);
    assert_eq!(std::fs::read(installed(&remote)).unwrap(), bytes);
    assert_eq!(mode(&installed(&remote)), 0o700);
    // 试跑的临时文件与错误输出都不留。
    let left: Vec<_> = std::fs::read_dir(installed(&remote).parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(left, [std::ffi::OsString::from("zcode-rs")]);
}

#[tokio::test]
async fn a_cached_executor_is_uploaded_without_downloading() {
    let remote = Remote::new();
    let file = executor(&remote, "cached", RUNS);
    let source = FakeSource::new(Some(file), Err("must not download".to_owned()));
    let provisioner = SshProvisioner::new(remote.target("dev-a"), wanted(), Vec::new())
        .with_source(source.clone());
    let (program, steps) = provision(&provisioner).await;
    assert_eq!(program.unwrap(), installed(&remote).to_string_lossy());
    assert!(
        matches!(steps.as_slice(), [ProvisionStep::Uploading { .. }]),
        "{steps:?}"
    );
    assert!(source.downloads().is_empty());
}

#[tokio::test]
async fn a_failed_download_falls_back_to_a_compatible_executor() {
    let remote = Remote::new();
    let older = older_candidate(&remote);
    let source = FakeSource::new(
        None,
        Err("GET https://registry.example.com/x: HTTP 404".to_owned()),
    );
    let provisioner = SshProvisioner::new(remote.target("dev-a"), wanted(), Vec::new())
        .with_source(source.clone());
    let (program, steps) = provision(&provisioner).await;
    assert_eq!(program.unwrap(), older.to_string_lossy());
    assert_eq!(steps, [ProvisionStep::Downloading]);
    assert_eq!(source.downloads().len(), 1);
    assert!(!installed(&remote).exists());
}

#[tokio::test]
async fn a_failed_download_without_a_compatible_executor_says_why() {
    let remote = Remote::new();
    let source = FakeSource::new(
        None,
        Err("GET https://registry.example.com/x: HTTP 404".to_owned()),
    );
    let provisioner =
        SshProvisioner::new(remote.target("dev-a"), wanted(), Vec::new()).with_source(source);
    let (program, _) = provision(&provisioner).await;
    assert_eq!(
        program,
        Err(ProvisionError::DownloadFailed {
            version: "0.2.0".to_owned(),
            target: "x86_64-unknown-linux-gnu".to_owned(),
            reason: "GET https://registry.example.com/x: HTTP 404".to_owned(),
        })
    );
}

#[tokio::test]
async fn a_musl_remote_is_refused_before_any_download() {
    let remote = Remote::new();
    let file = executor(&remote, "downloaded", RUNS);
    let source = FakeSource::new(Some(file.clone()), Ok(file));
    let provisioner = SshProvisioner::new(remote.target("alpine"), wanted(), Vec::new())
        .with_source(source.clone());
    let (program, steps) = provision(&provisioner).await;
    assert_eq!(program, Err(ProvisionError::Musl));
    assert!(steps.is_empty(), "{steps:?}");
    assert!(source.downloads().is_empty());
    assert_eq!(source.cached_calls.load(Ordering::SeqCst), 0);
    let message = ProvisionError::Musl.to_string();
    assert!(
        message.contains("musl") && message.contains("Alpine"),
        "{message}"
    );

    // 远端的候选与本机的静态产物不受这条限制。
    let older = older_candidate(&remote);
    let (program, _) = provision(&provisioner).await;
    assert_eq!(program.unwrap(), older.to_string_lossy());
    let remote = Remote::new();
    let (artifact, bytes) = remote.artifact();
    let provisioner = SshProvisioner::new(remote.target("alpine"), wanted(), vec![artifact])
        .with_source(source.clone());
    let (program, _) = provision(&provisioner).await;
    assert_eq!(program.unwrap(), installed(&remote).to_string_lossy());
    assert_eq!(std::fs::read(installed(&remote)).unwrap(), bytes);
    assert!(source.downloads().is_empty());
}

#[tokio::test]
async fn an_executor_that_needs_a_newer_glibc_names_the_remote_version() {
    let remote = Remote::new();
    let file = executor(
        &remote,
        "downloaded",
        "echo \"$0: /lib/x86_64-linux-gnu/libc.so.6: version \\`GLIBC_2.34' not found (required by $0)\" >&2\nexit 1",
    );
    let source = FakeSource::new(None, Ok(file));
    let provisioner =
        SshProvisioner::new(remote.target("dev-a"), wanted(), Vec::new()).with_source(source);
    let (program, _) = provision(&provisioner).await;
    let error = program.unwrap_err();
    assert_eq!(
        error,
        ProvisionError::OldGlibc {
            glibc: Some("2.31".to_owned()),
            needs: "GLIBC_2.34".to_owned(),
        }
    );
    assert_eq!(
        error.to_string(),
        "the zcode executor needs GLIBC_2.34, but the remote has glibc 2.31"
    );
    // 改名之前试跑：跑不起来的执行器不留在远端。
    let dir = installed(&remote).parent().unwrap().to_path_buf();
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
}

#[tokio::test]
async fn a_local_static_artifact_wins_over_the_source_and_is_not_trial_run() {
    let remote = Remote::new();
    // ELF 头加填充：在本机试跑一定失败，所以上传成功就说明没有试跑。
    let (artifact, bytes) = remote.artifact();
    let source = FakeSource::new(None, Err("must not download".to_owned()));
    let provisioner = SshProvisioner::new(remote.target("dev-a"), wanted(), vec![artifact])
        .with_source(source.clone());
    let (program, steps) = provision(&provisioner).await;
    assert_eq!(program.unwrap(), installed(&remote).to_string_lossy());
    assert_eq!(std::fs::read(installed(&remote)).unwrap(), bytes);
    assert!(
        matches!(steps.as_slice(), [ProvisionStep::Uploading { .. }]),
        "{steps:?}"
    );
    assert!(source.downloads().is_empty());
    assert_eq!(source.cached_calls.load(Ordering::SeqCst), 0);
}
