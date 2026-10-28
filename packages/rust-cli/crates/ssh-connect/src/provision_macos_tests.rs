//! F107（remote-exec.md §28）：macOS 的远端——下载的产物上传后试跑、试跑被系统杀掉时报签名无效、Intel Mac 在下载之前
//! 就报错（远端已有兼容的执行器时照样用它）、本机的 Mach-O 静态产物不试跑。远端是假的 `ssh`
//! （`provision_tests.rs` 的 `Remote::macos`：`uname` 说 Darwin，命令在本机的 `sh` 上跑）。
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "测试直接准备文件系统夹具与进程"
)]

use std::sync::atomic::Ordering;

use rust_cli_exec_transport::{ProvisionError, ProvisionStep};

use crate::SshProvisioner;
use crate::artifact::Platform;
use crate::provision_source_tests::{
    FakeSource, RUNS, executor, installed, older_candidate, provision,
};
use crate::provision_tests::{Remote, mode, wanted};

/// 签名无效的样子：系统直接 SIGKILL（shell 看到 137）。
const KILLED: &str = "kill -9 $$";

/// F107（§28.2、§28.8）：macOS arm64 的远端照 Linux 的路走——下载 `darwin-arm64` 的产物、上传到 `bin/<版本>/`、试跑。
#[tokio::test]
async fn a_macos_remote_downloads_uploads_and_trial_runs_the_executor() {
    let remote = Remote::macos("arm64");
    let file = executor(&remote, "downloaded", RUNS);
    let source = FakeSource::new(None, Ok(file.clone()));
    let provisioner = SshProvisioner::new(remote.target("mac-1"), wanted(), Vec::new())
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
    // 下载问的是 macOS 的平台（包 id `darwin-arm64`，缓存键 `aarch64-apple-darwin`）。
    assert_eq!(
        source.downloads(),
        [("0.2.0".to_owned(), Platform::MacosAarch64)]
    );
    assert_eq!(std::fs::read(installed(&remote)).unwrap(), bytes);
    assert_eq!(mode(&installed(&remote)), 0o700);
    // 试跑的临时文件与它的输出都不留。
    let left: Vec<_> = std::fs::read_dir(installed(&remote).parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(left, [std::ffi::OsString::from("zcode-rs")]);
}

/// F107（§28.5）：试跑被系统杀掉（SIGKILL）时说签名无效、写明办法，远端不留文件；Linux 的远端上同样的退出码照旧是
/// 上传失败（那不是签名的事）。
#[tokio::test]
async fn an_executor_killed_on_a_macos_remote_says_the_signature_is_not_valid() {
    let remote = Remote::macos("aarch64");
    let file = executor(&remote, "unsigned", KILLED);
    let source = FakeSource::new(None, Ok(file.clone()));
    let provisioner = SshProvisioner::new(remote.target("mac-1"), wanted(), Vec::new())
        .with_source(source.clone());
    let (program, _) = provision(&provisioner).await;
    assert_eq!(program, Err(ProvisionError::Unsigned));
    let message = ProvisionError::Unsigned.to_string();
    assert!(
        message.contains("killed") && message.contains("signature"),
        "{message}"
    );
    // 改名之前试跑：跑不起来的执行器不留在远端。
    let dir = installed(&remote).parent().unwrap().to_path_buf();
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);

    // 同一个脚本在 Linux 的远端上：137 与签名无关，照旧 `Upload`。
    let linux = Remote::new();
    let file = executor(&linux, "oom", KILLED);
    let provisioner = SshProvisioner::new(linux.target("dev-a"), wanted(), Vec::new())
        .with_source(FakeSource::new(None, Ok(file)));
    let (program, _) = provision(&provisioner).await;
    assert!(
        matches!(program, Err(ProvisionError::Upload(_))),
        "{program:?}"
    );
}

/// F107（§28.2）：Intel Mac 没有发布的包——一个请求都不发就报 `NoPackage`，写明应该有的包与办法；远端已经装了协议
/// 相同的执行器时照样用它（与 musl 的规则相同）。
#[tokio::test]
async fn an_intel_mac_is_refused_before_any_download_but_uses_what_is_there() {
    let remote = Remote::macos("x86_64");
    let source = FakeSource::new(None, Err("must not download"));
    let provisioner = SshProvisioner::new(remote.target("mac-intel"), wanted(), Vec::new())
        .with_source(source.clone());
    let (program, steps) = provision(&provisioner).await;
    assert_eq!(
        program,
        Err(ProvisionError::NoPackage {
            package: "darwin-x64".to_owned()
        })
    );
    assert!(steps.is_empty(), "{steps:?}");
    assert!(source.downloads().is_empty());
    assert_eq!(source.cached_calls.load(Ordering::SeqCst), 0);

    // 远端自己装了协议相同的执行器（例如 `npm i -g`）：用它，不下载。
    let older = older_candidate(&remote);
    let (program, _) = provision(&provisioner).await;
    assert_eq!(program.unwrap(), older.to_string_lossy());
    assert!(source.downloads().is_empty());
}

/// F107（§28.4、§28.8）：本机的 Mach-O 静态产物（测试变量给的）排在来源之前，上传时不试跑；Linux 的 ELF 产物不会被
/// 当成 macOS 的产物。
#[tokio::test]
async fn a_local_mach_o_artifact_wins_over_the_source_and_is_not_trial_run() {
    let remote = Remote::macos("arm64");
    // Mach-O 头加填充：在本机试跑一定失败，所以上传成功就说明没有试跑。
    let (darwin, bytes) = remote.macho_artifact();
    let (linux, _) = remote.artifact();
    let source = FakeSource::new(None, Err("must not download"));
    let provisioner = SshProvisioner::new(
        remote.target("mac-1"),
        wanted(),
        vec![linux, darwin.clone()],
    )
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

/// F107（§28.2）：探测到 Windows 照旧报 `Unsupported`，文字写明支持哪些平台。
#[tokio::test]
async fn a_windows_remote_is_still_unsupported() {
    let windows = Remote::saying("MINGW64_NT-10.0-22631", "x86_64");
    let source = FakeSource::new(None, Err("must not download"));
    let provisioner = SshProvisioner::new(windows.target("win-1"), wanted(), Vec::new())
        .with_source(source.clone());
    let (program, _) = provision(&provisioner).await;
    assert_eq!(
        program,
        Err(ProvisionError::Unsupported {
            os: "MINGW64_NT-10.0-22631".to_owned(),
            arch: "x86_64".to_owned()
        })
    );
    assert!(source.downloads().is_empty());
}
