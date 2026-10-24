//! 部署器与连接器，经一个假的 `ssh`（在本机跑远端命令，HOME 是临时目录，`uname` 说自己是 Linux x86_64）：上传并校验、
//! 已有同版本时不传、sha256 不对时不留文件、连不上时报 Unreachable、连接器的命令转义。
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "测试直接准备文件系统夹具与进程"
)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use rust_cli_exec_transport::{Connector, ProvisionError, Provisioned, Provisioner};

use crate::{SshConnector, SshProvisioner, SshTarget, Wanted};

struct Remote {
    root: tempfile::TempDir,
}

impl Remote {
    fn new() -> Self {
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
            "#!/bin/sh\ncase \"$1\" in -s) echo Linux;; -m) echo x86_64;; esac\n",
        );
        // 假的 ssh：跳过选项与目标，在本机用 sh 跑远端命令（同真实 ssh：命令串交给远端的登录 shell）。
        write(
            "ssh",
            "#!/bin/sh\nwhile [ $# -gt 0 ]; do case \"$1\" in -F|-o) shift 2;; -T) shift;; *) break;; esac; done\n\
             [ \"$1\" = unreachable ] && { echo 'Permission denied (publickey).' >&2; exit 255; }\n\
             shift\ncd \"$HOME\" && exec /bin/sh -c \"$*\"\n",
        );
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn target(&self, destination: &str) -> SshTarget {
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

    /// 一个 x86_64 的 Linux ELF 头加一些内容。
    fn artifact(&self) -> (PathBuf, Vec<u8>) {
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

fn wanted() -> Wanted {
    Wanted {
        version: "0.2.0".to_owned(),
        protocol: 1,
    }
}

fn mode(path: &Path) -> u32 {
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
