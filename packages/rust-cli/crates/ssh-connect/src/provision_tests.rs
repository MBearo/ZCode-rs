//! 部署器与连接器，经一个假的 `ssh`（在本机跑远端命令，HOME 是临时目录，`uname` 说自己是 Linux x86_64）：上传并校验、
//! 已有同版本时不传、sha256 不对时不留文件、连不上时报 Unreachable、连接器的命令转义。
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "测试直接准备文件系统夹具与进程"
)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use rust_cli_exec_transport::{Connector, LinkError, ProvisionError, Provisioned, Provisioner};

use crate::{SshConnector, SshProvisioner, SshTarget, Wanted};

/// 假的远端（F100 的测试也用：`provision_source_tests.rs`）。
pub(crate) struct Remote {
    pub(crate) root: tempfile::TempDir,
}

impl Remote {
    pub(crate) fn new() -> Self {
        Self::saying("Linux", "x86_64")
    }

    /// F107（§28.2）：假的 macOS 远端（`uname` 说 Darwin；脚本不会在 macOS 上问 libc）。
    pub(crate) fn macos(arch: &str) -> Self {
        Self::saying("Darwin", arch)
    }

    /// `uname -s` / `uname -m` 说什么的假远端。
    pub(crate) fn saying(os: &str, machine: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let fake = root.path().join("fake");
        std::fs::create_dir_all(&fake).unwrap();
        std::fs::create_dir_all(root.path().join("home")).unwrap();
        let write = |name: &str, text: &str| {
            let path = fake.join(name);
            std::fs::write(&path, text).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        write(
            "uname",
            &format!("#!/bin/sh\ncase \"$1\" in -s) echo {os};; -m) echo {machine};; esac\n"),
        );
        // F100：glibc 2.31；目标 alpine 是 musl（没有 glibc，探测多一行 `libc musl`：本机没有 /lib/ld-musl-*）。
        write(
            "getconf",
            "#!/bin/sh\n[ \"$FAKE_LIBC\" = musl ] && exit 1\n[ \"$1\" = GNU_LIBC_VERSION ] && echo 'glibc 2.31'\n",
        );
        // 假的 ssh：跳过选项与目标，在本机用 sh 跑远端命令（同真实 ssh：命令串交给远端的登录 shell）。
        write(
            "ssh",
            "#!/bin/sh\nwhile [ $# -gt 0 ]; do case \"$1\" in -F|-o) shift 2;; -T) shift;; *) break;; esac; done\n\
             [ \"$1\" = unreachable ] && { echo 'Permission denied (publickey).' >&2; exit 255; }\n\
             [ \"$1\" = down ] && { echo 'ssh: connect to host down port 22: Connection refused' >&2; exit 255; }\n\
             if [ \"$1\" = alpine ]; then export FAKE_LIBC=musl; shift; cd \"$HOME\" || exit 1\n\
             /bin/sh -c \"$*\"; code=$?; [ \"$*\" = 'sh -s' ] && echo 'libc musl'; exit $code; fi\n\
             shift\ncd \"$HOME\" && exec /bin/sh -c \"$*\"\n",
        );
        Self { root }
    }

    pub(crate) fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    pub(crate) fn target(&self, destination: &str) -> SshTarget {
        let fake = self.root.path().join("fake");
        SshTarget {
            destination: destination.to_owned(),
            program: fake.join("ssh"),
            config: None,
            options: Vec::new(),
            env: vec![
                (
                    "HOME".to_owned(),
                    self.home().to_string_lossy().into_owned(),
                ),
                (
                    "PATH".to_owned(),
                    format!("{}:/usr/bin:/bin", fake.display()),
                ),
            ],
        }
    }

    /// 一个 arm64 的 Mach-O 头加一些内容（F107，§28.4：macOS 的远端认这种产物）。
    pub(crate) fn macho_artifact(&self) -> (PathBuf, Vec<u8>) {
        let mut bytes = vec![0u8; 64];
        bytes[..4].copy_from_slice(&0xfeed_facfu32.to_le_bytes());
        bytes[4..8].copy_from_slice(&0x0100_000cu32.to_le_bytes());
        bytes.extend_from_slice(&[7u8; 5000]);
        let path = self.root.path().join("zcode-rs-darwin");
        std::fs::write(&path, &bytes).unwrap();
        (path, bytes)
    }

    /// 一个 x86_64 的 Linux ELF 头加一些内容。
    pub(crate) fn artifact(&self) -> (PathBuf, Vec<u8>) {
        let mut bytes = vec![0u8; 64];
        bytes[..6].copy_from_slice(b"\x7fELF\x02\x01");
        bytes[16] = 2;
        bytes[18] = 62;
        bytes.extend_from_slice(&[7u8; 5000]);
        let path = self.root.path().join("zcode-rs");
        std::fs::write(&path, &bytes).unwrap();
        (path, bytes)
    }
}

pub(crate) fn wanted() -> Wanted {
    Wanted {
        version: "0.2.0".to_owned(),
        protocol: 1,
    }
}

pub(crate) fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[tokio::test]
async fn the_executor_is_uploaded_verified_and_used() {
    let remote = Remote::new();
    let (artifact, bytes) = remote.artifact();
    // 另一种架构的产物不会被选上。
    let other = remote.root.path().join("other");
    let mut arm = bytes.clone();
    arm[18] = 183;
    std::fs::write(&other, arm).unwrap();
    let provisioner = SshProvisioner::new(remote.target("dev-a"), wanted(), vec![other, artifact]);
    let provisioned = provisioner.provision().await.unwrap();
    let installed = remote
        .home()
        .join(".zcode/rust-cli/exec/bin/0.2.0/zcode-rs");
    assert_eq!(
        provisioned,
        Provisioned {
            program: installed.to_string_lossy().into_owned(),
            args: vec!["executor".to_owned(), "--stdio".to_owned()],
        }
    );
    assert_eq!(std::fs::read(&installed).unwrap(), bytes);
    assert_eq!(mode(&installed), 0o700);
    assert_eq!(mode(&remote.home().join(".zcode/rust-cli/exec")), 0o700);
    // 没有留下临时文件。
    let left: Vec<_> = std::fs::read_dir(installed.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(left, [std::ffi::OsString::from("zcode-rs")]);
}

#[tokio::test]
async fn an_installed_executor_of_the_same_version_is_used_without_uploading() {
    let remote = Remote::new();
    let installed = remote
        .home()
        .join(".zcode/rust-cli/exec/bin/0.2.0/zcode-rs");
    std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
    std::fs::write(
        &installed,
        "#!/bin/sh\necho '{\"version\":\"0.2.0\",\"protocol\":1,\"resident\":null}'\n",
    )
    .unwrap();
    std::fs::set_permissions(&installed, std::fs::Permissions::from_mode(0o700)).unwrap();
    // 没有本机产物也行：远端已经有了。
    let provisioner = SshProvisioner::new(remote.target("dev-a"), wanted(), Vec::new());
    let provisioned = provisioner.provision().await.unwrap();
    assert_eq!(provisioned.program, installed.to_string_lossy());
}

#[tokio::test]
async fn a_wrong_digest_leaves_no_executor_behind() {
    let remote = Remote::new();
    let dir = remote.home().join("bin");
    let output = tokio::process::Command::new("/bin/sh")
        .args(["-c", &crate::provision::upload_script(), "sh"])
        .arg(&dir)
        .arg("0".repeat(64))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let output = {
        let mut child = output;
        drop(child.stdin.take());
        child.wait_with_output().await.unwrap()
    };
    assert_eq!(output.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&output.stderr).contains("sha256 mismatch"));
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
}

#[tokio::test]
async fn without_an_artifact_or_access_the_errors_are_typed() {
    let remote = Remote::new();
    let provisioner = SshProvisioner::new(remote.target("dev-a"), wanted(), Vec::new());
    assert_eq!(
        provisioner.provision().await,
        Err(ProvisionError::NoArtifact {
            version: "0.2.0".to_owned(),
            target: "x86_64-unknown-linux-musl".to_owned()
        })
    );
    let denied = SshProvisioner::new(remote.target("unreachable"), wanted(), Vec::new());
    let Err(ProvisionError::Unreachable { target, message }) = denied.provision().await else {
        panic!("expected unreachable");
    };
    assert_eq!(target, "unreachable");
    assert!(message.contains("Permission denied"), "{message}");
}

#[tokio::test]
async fn the_connector_runs_the_provisioned_command_over_ssh() {
    let remote = Remote::new();
    let provisioned = Provisioned {
        program: "/bin/sh".to_owned(),
        args: vec!["-c".to_owned(), "exec cat # it's quoted".to_owned()],
    };
    let connector = SshConnector::new(remote.target("dev-a"), &provisioned).unwrap();
    assert_eq!(connector.describe(), "ssh:dev-a");
    let link = connector.connect().await.unwrap();
    let (mut tx, mut rx) = link.split();
    tx.send(b"frame".to_vec().into()).await.unwrap();
    assert_eq!(rx.recv().await.unwrap().unwrap().as_ref(), b"frame");
    tx.close().await.unwrap();
    assert_eq!(rx.recv().await.unwrap(), None);
}

/// 连一次，读到连接结束时的结果（ssh 起来了、远端失败时错误在接收方向，§18.6 的收尾顺序）。
async fn ended(
    remote: &Remote,
    destination: &str,
    program: &str,
) -> Result<Option<usize>, LinkError> {
    let provisioned = Provisioned {
        program: program.to_owned(),
        args: vec!["executor".to_owned(), "--stdio".to_owned()],
    };
    let connector = SshConnector::new(remote.target(destination), &provisioned).unwrap();
    let link = connector.connect().await.unwrap();
    let (_tx, mut rx) = link.split();
    rx.recv().await.map(|frame| frame.map(|bytes| bytes.len()))
}

/// F96（remote-exec.md §23.4）：认证失败与远端的执行器程序不在了，再连也一样：连接器标成 `Rejected`（exec-client 不再
/// 重连）；连不上之类可恢复的照旧是普通的错误。
#[tokio::test]
async fn ssh_failures_that_retrying_cannot_fix_are_rejected() {
    let remote = Remote::new();
    let denied = ended(&remote, "unreachable", "/bin/true").await;
    assert!(
        matches!(&denied, Err(LinkError::Rejected(message)) if message.contains("Permission denied")),
        "{denied:?}"
    );
    let missing = ended(&remote, "dev-a", "/nowhere/zcode-rs").await;
    assert!(
        matches!(&missing, Err(LinkError::Rejected(message)) if message.contains("/nowhere/zcode-rs")),
        "{missing:?}"
    );
    let down = ended(&remote, "down", "/bin/true").await;
    assert!(
        matches!(&down, Err(LinkError::Io(message)) if message.contains("Connection refused")),
        "{down:?}"
    );
}

#[test]
fn the_rejection_rules_follow_the_ssh_messages() {
    use crate::connector::rejected;
    for message in [
        "executor exited (exit status: 255): Permission denied (publickey).\n",
        "executor exited (exit status: 255): Received disconnect from 10.0.0.2 port 22:2: Too many authentication failures\n",
        "executor exited (exit status: 255): Host key verification failed.\n",
        "executor exited (exit status: 255): @@@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @@@\n",
        "executor exited (exit status: 127): sh: /root/.zcode/rust-cli/exec/bin/x/zcode-rs: not found\n",
        "executor exited (exit status: 126): sh: /root/x/zcode-rs: Permission denied\n",
    ] {
        assert!(rejected(message), "{message}");
    }
    for message in [
        "executor exited (exit status: 255): ssh: connect to host dev-a port 22: Connection refused\n",
        "executor exited (exit status: 255): ssh: connect to host dev-a port 22: Operation timed out\n",
        "executor exited (exit status: 255): ssh: Could not resolve hostname dev-a: nodename nor servname provided\n",
        "executor exited (exit status: 255): Connection closed by 10.0.0.2 port 22\n",
        "executor exited (exit status: 1): zcode executor: cannot start the resident executor: busy\n",
        "link closed",
    ] {
        assert!(!rejected(message), "{message}");
    }
}

/// F98（remote-exec.md §25.4.2）：要上传时，上传之前报一次大小（状态行写“上传执行器（N MB）”）；远端已经有兼容的执行器时
/// 不报。旧代码没有部署的进度。
#[tokio::test]
async fn uploading_reports_its_size_first() {
    use rust_cli_exec_transport::ProvisionStep;
    let seen = std::sync::Mutex::new(Vec::new());
    let record = |step: ProvisionStep| seen.lock().unwrap().push(step);
    let remote = Remote::new();
    let (artifact, bytes) = remote.artifact();
    let provisioner = SshProvisioner::new(remote.target("dev-a"), wanted(), vec![artifact]);
    provisioner.provision_reporting(&record).await.unwrap();
    assert_eq!(
        *seen.lock().unwrap(),
        [ProvisionStep::Uploading {
            bytes: bytes.len() as u64
        }]
    );
    // 远端已经有同一版本的执行器：不上传、不报。
    seen.lock().unwrap().clear();
    let remote = Remote::new();
    let installed = remote
        .home()
        .join(".zcode/rust-cli/exec/bin/0.2.0/zcode-rs");
    std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
    std::fs::write(
        &installed,
        "#!/bin/sh\necho '{\"version\":\"0.2.0\",\"protocol\":1,\"resident\":null}'\n",
    )
    .unwrap();
    std::fs::set_permissions(&installed, std::fs::Permissions::from_mode(0o700)).unwrap();
    SshProvisioner::new(remote.target("dev-a"), wanted(), Vec::new())
        .provision_reporting(&record)
        .await
        .unwrap();
    assert!(seen.lock().unwrap().is_empty());
}
