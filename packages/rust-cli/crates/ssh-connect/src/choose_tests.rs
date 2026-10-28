//! remote-exec.md §19.9 的选用表：每一行一个用例。

use rust_cli_exec_transport::ProvisionError;

use crate::artifact::Platform;
use crate::choose::{Plan, Wanted, choose};
use crate::probe::{Candidate, Probe, Report, Resident};

fn wanted() -> Wanted {
    Wanted {
        version: "0.2.0".to_owned(),
        protocol: 1,
    }
}

fn report(version: &str, protocol: u32, resident: Option<Resident>) -> Option<Report> {
    Some(Report {
        version: version.to_owned(),
        protocol,
        executable: None,
        resident,
    })
}

fn resident(version: &str, protocol: u32, idle: bool) -> Resident {
    Resident {
        version: version.to_owned(),
        protocol,
        idle,
        executable: Some(format!("/home/u/bin/{version}/zcode-rs")),
    }
}

fn probe(candidates: Vec<(&str, Option<Report>)>) -> Probe {
    Probe {
        os: "Linux".to_owned(),
        arch: "x86_64".to_owned(),
        home: "/home/u".to_owned(),
        hashes: vec!["sha256sum".to_owned()],
        libc: Default::default(),
        candidates: candidates
            .into_iter()
            .map(|(path, report)| Candidate {
                path: path.to_owned(),
                report,
            })
            .collect(),
    }
}

#[test]
fn other_systems_and_architectures_are_unsupported() {
    // F107（§28.2）：Windows、别的 Unix 与认不出来的架构照旧不支持；macOS 见下一个用例。
    for (os, arch) in [
        ("MINGW64_NT-10.0-22631", "x86_64"),
        ("CYGWIN_NT-10.0", "x86_64"),
        ("Linux", "riscv64"),
        ("FreeBSD", "amd64"),
        ("Darwin", "i386"),
    ] {
        let mut remote = probe(Vec::new());
        remote.os = os.to_owned();
        remote.arch = arch.to_owned();
        assert_eq!(
            choose(&remote, &wanted(), true),
            Err(ProvisionError::Unsupported {
                os: os.to_owned(),
                arch: arch.to_owned()
            })
        );
    }
    let mut arm = probe(Vec::new());
    arm.arch = "aarch64".to_owned();
    assert_eq!(
        choose(&arm, &wanted(), true),
        Ok(Plan::Upload(Platform::LinuxAarch64))
    );
}

/// F107（§28.2）：macOS 的远端走同一张表；Intel Mac 认得出来，只是没有可上传的产物时 target 写它自己的。
#[test]
fn macos_remotes_go_through_the_same_table() {
    let macos = |arch: &str, candidates: Vec<(&str, Option<Report>)>| {
        let mut remote = probe(candidates);
        remote.os = "Darwin".to_owned();
        remote.arch = arch.to_owned();
        remote.home = "/Users/dev".to_owned();
        remote
    };
    // 远端什么都没有、本机有产物：上传。
    for arch in ["arm64", "aarch64"] {
        assert_eq!(
            choose(&macos(arch, Vec::new()), &wanted(), true),
            Ok(Plan::Upload(Platform::MacosAarch64))
        );
    }
    // 没有可上传的产物：target 是这个平台自己的（不是 musl 的目录）。
    for (arch, target) in [
        ("arm64", "aarch64-apple-darwin"),
        ("x86_64", "x86_64-apple-darwin"),
    ] {
        assert_eq!(
            choose(&macos(arch, Vec::new()), &wanted(), false),
            Err(ProvisionError::NoArtifact {
                version: "0.2.0".to_owned(),
                target: target.to_owned()
            })
        );
    }
    // 远端已有同版本的候选：直接用（Intel Mac 也是这条路，例如那台机器上自己装了 zcode）。
    let exact = macos("x86_64", vec![("/npm/zcode-rs", report("0.2.0", 1, None))]);
    assert_eq!(
        choose(&exact, &wanted(), false),
        Ok(Plan::Use("/npm/zcode-rs".to_owned()))
    );
    // 协议相同、版本不同的候选：没有产物时用它。
    let compatible = macos("arm64", vec![("/npm/zcode-rs", report("0.1.9", 1, None))]);
    assert_eq!(
        choose(&compatible, &wanted(), false),
        Ok(Plan::Use("/npm/zcode-rs".to_owned()))
    );
    // 常驻进程在跑、版本相同：用它自己的二进制。
    let running = macos(
        "arm64",
        vec![(
            "/Users/dev/.zcode/rust-cli/exec/bin/0.2.0/zcode-rs",
            report("0.2.0", 1, Some(resident("0.2.0", 1, true))),
        )],
    );
    assert_eq!(
        choose(&running, &wanted(), true),
        Ok(Plan::Use("/home/u/bin/0.2.0/zcode-rs".to_owned()))
    );
}

#[test]
fn without_a_resident_the_same_version_then_an_upload_then_a_compatible_one() {
    let exact = probe(vec![
        ("/npm/zcode-rs", report("0.1.9", 1, None)),
        ("/home/u/bin/0.2.0/zcode-rs", report("0.2.0", 1, None)),
    ]);
    assert_eq!(
        choose(&exact, &wanted(), true),
        Ok(Plan::Use("/home/u/bin/0.2.0/zcode-rs".to_owned()))
    );
    let npm = probe(vec![
        ("/old/zcode-rs", None),
        ("/npm/zcode-rs", report("0.1.9", 1, None)),
        ("/future/zcode-rs", report("0.9.0", 2, None)),
    ]);
    assert_eq!(
        choose(&npm, &wanted(), true),
        Ok(Plan::Upload(Platform::LinuxX86_64))
    );
    assert_eq!(
        choose(&npm, &wanted(), false),
        Ok(Plan::Use("/npm/zcode-rs".to_owned()))
    );
    let nothing = probe(vec![("/future/zcode-rs", report("0.9.0", 2, None))]);
    assert_eq!(
        choose(&nothing, &wanted(), false),
        Err(ProvisionError::NoArtifact {
            version: "0.2.0".to_owned(),
            target: "x86_64-unknown-linux-musl".to_owned()
        })
    );
}

#[test]
fn a_resident_of_the_same_version_is_used() {
    let remote = probe(vec![(
        "/npm/zcode-rs",
        report("0.1.9", 1, Some(resident("0.2.0", 1, false))),
    )]);
    assert_eq!(
        choose(&remote, &wanted(), true),
        Ok(Plan::Use("/home/u/bin/0.2.0/zcode-rs".to_owned()))
    );
}

#[test]
fn a_busy_compatible_resident_of_another_version_is_kept() {
    let remote = probe(vec![(
        "/home/u/bin/0.1.0/zcode-rs",
        report("0.1.0", 1, Some(resident("0.1.0", 1, false))),
    )]);
    assert_eq!(
        choose(&remote, &wanted(), true),
        Ok(Plan::Use("/home/u/bin/0.1.0/zcode-rs".to_owned()))
    );
}

#[test]
fn an_idle_resident_of_another_version_is_replaced_when_possible() {
    let idle = Some(resident("0.1.0", 1, true));
    let remote = probe(vec![(
        "/home/u/bin/0.1.0/zcode-rs",
        report("0.1.0", 1, idle),
    )]);
    assert_eq!(
        choose(&remote, &wanted(), true),
        Ok(Plan::Upload(Platform::LinuxX86_64))
    );
    // 没有本机产物：协议相同就继续用它。
    assert_eq!(
        choose(&remote, &wanted(), false),
        Ok(Plan::Use("/home/u/bin/0.1.0/zcode-rs".to_owned()))
    );
    // 协议不同而空闲：没有产物也没有兼容的二进制时报不兼容（不忙）。
    let other = Some(resident("0.9.0", 2, true));
    let remote = probe(vec![("/future/zcode-rs", report("0.9.0", 2, other))]);
    assert_eq!(
        choose(&remote, &wanted(), true),
        Ok(Plan::Upload(Platform::LinuxX86_64))
    );
    assert_eq!(
        choose(&remote, &wanted(), false),
        Err(ProvisionError::Incompatible {
            version: "0.9.0".to_owned(),
            protocol: 2,
            busy: false
        })
    );
}

#[test]
fn a_busy_incompatible_resident_is_refused() {
    let busy = Some(resident("0.9.0", 2, false));
    let remote = probe(vec![("/future/zcode-rs", report("0.9.0", 2, busy))]);
    assert_eq!(
        choose(&remote, &wanted(), true),
        Err(ProvisionError::Incompatible {
            version: "0.9.0".to_owned(),
            protocol: 2,
            busy: true
        })
    );
}
