//! 探测：输出的解析；脚本在本机的 `sh` 上跑（临时 HOME 里放假的候选），只执行名字是 `zcode-rs` 的文件。
#![allow(clippy::disallowed_methods, reason = "测试直接准备文件系统夹具与进程")]

use crate::probe::{Probe, Report, Resident};
use crate::target::{SshTarget, quote};

#[test]
fn the_probe_output_is_parsed() {
    let output = "os Linux\narch aarch64\nhome /home/u\nhash sha256sum\nhash openssl\n\
candidate /home/u/.zcode/rust-cli/exec/bin/0.1.0/zcode-rs\t{\"version\":\"0.1.0\",\"protocol\":1,\"executable\":\"/x\",\"resident\":{\"version\":\"0.1.0\",\"protocol\":1,\"idle\":true,\"pid\":3,\"later\":1}}\n\
candidate /usr/bin/zcode-rs\t\n\
candidate /usr/bin/zcode-rs\tnot json\n";
    let probe = Probe::parse(output).unwrap();
    assert_eq!(
        (probe.os.as_str(), probe.arch.as_str()),
        ("Linux", "aarch64")
    );
    assert_eq!(probe.hashes, ["sha256sum", "openssl"]);
    assert_eq!(probe.candidates.len(), 2, "duplicates are dropped");
    assert_eq!(
        probe.candidates[0].report,
        Some(Report {
            version: "0.1.0".to_owned(),
            protocol: 1,
            executable: Some("/x".to_owned()),
            resident: Some(Resident {
                version: "0.1.0".to_owned(),
                protocol: 1,
                idle: true,
                executable: None,
            }),
        })
    );
    assert_eq!(probe.candidates[1].report, None);
    assert!(probe.resident().unwrap().idle);
    assert_eq!(
        probe.binary_path("0.2.0"),
        "/home/u/.zcode/rust-cli/exec/bin/0.2.0/zcode-rs"
    );
    assert!(Probe::parse("Welcome to fish\n").is_err());
}

#[test]
fn ssh_arguments_and_quoting() {
    let target = SshTarget {
        destination: "dev-a".to_owned(),
        program: "ssh".into(),
        config: Some("/tmp/cfg".into()),
        options: vec!["UserKnownHostsFile=/tmp/kh".to_owned()],
        env: Vec::new(),
    };
    assert_eq!(
        target.args("sh -s").unwrap(),
        [
            "-F",
            "/tmp/cfg",
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "UserKnownHostsFile=/tmp/kh",
            "dev-a",
            "sh -s"
        ]
    );
    let bad = SshTarget {
        destination: "-oProxyCommand=x".to_owned(),
        ..target
    };
    assert!(bad.args("x").is_err());
    assert_eq!(quote("it's"), "'it'\\''s'");
}

#[cfg(unix)]
#[tokio::test]
async fn the_script_finds_ours_and_the_npm_one_and_never_runs_zcode() {
    use std::os::unix::fs::PermissionsExt;
    // F94：`script` 只有这个 Unix 的测试用（在本机的 `sh` 上跑），放在文件头时 Windows 上是没用到的导入。
    use crate::probe::script;
    let home = tempfile::tempdir().unwrap();
    let write = |path: &std::path::Path, text: &str| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    let exec = home.path().join(".zcode/rust-cli/exec/bin");
    write(
        &exec.join("0.1.0/zcode-rs"),
        "#!/bin/sh\n[ \"$*\" = 'executor status --json' ] && echo '{\"version\":\"0.1.0\",\"protocol\":1,\"resident\":null}'\n",
    );
    // 老版本：不认 `status --json`，报错退出。
    write(
        &exec.join("0.0.1/zcode-rs"),
        "#!/bin/sh\necho 'error: unknown option' >&2; exit 1\n",
    );
    // npm 装的：`zcode` 是包里启动器的链接，平台包在包的 node_modules 里。
    let package = home.path().join("prefix/lib/node_modules/@scope/zcode");
    write(
        &package.join("bin/zcode.js"),
        "#!/bin/sh\ntouch \"$HOME/ran-zcode\"\n",
    );
    write(
        &package.join("node_modules/@scope/zcode-linux-x64/bin/zcode-rs"),
        "#!/bin/sh\necho '{\"version\":\"0.1.5\",\"protocol\":1}'\n",
    );
    let bin = home.path().join("prefix/bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(package.join("bin/zcode.js"), bin.join("zcode")).unwrap();
    let mut child = tokio::process::Command::new("/bin/sh")
        .arg("-s")
        .env_clear()
        .env("HOME", home.path())
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    {
        use tokio::io::AsyncWriteExt;
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(script().as_bytes()).await.unwrap();
    }
    let output = child.wait_with_output().await.unwrap();
    assert!(output.status.success());
    let probe = Probe::parse(&String::from_utf8_lossy(&output.stdout)).unwrap();
    assert!(!probe.hashes.is_empty());
    let found: Vec<(String, Option<String>)> = probe
        .candidates
        .iter()
        .map(|candidate| {
            let name = candidate
                .path
                .trim_start_matches(&*home.path().to_string_lossy())
                .to_owned();
            (
                name,
                candidate
                    .report
                    .as_ref()
                    .map(|report| report.version.clone()),
            )
        })
        .collect();
    assert!(
        found.contains(&(
            "/.zcode/rust-cli/exec/bin/0.1.0/zcode-rs".to_owned(),
            Some("0.1.0".to_owned())
        )),
        "{found:?}"
    );
    assert!(
        found.contains(&("/.zcode/rust-cli/exec/bin/0.0.1/zcode-rs".to_owned(), None)),
        "{found:?}"
    );
    assert!(
        found.iter().any(
            |(path, version)| path.ends_with("zcode-linux-x64/bin/zcode-rs")
                && version.as_deref() == Some("0.1.5")
        ),
        "{found:?}"
    );
    assert!(
        !home.path().join("ran-zcode").exists(),
        "zcode itself is never run"
    );
}
