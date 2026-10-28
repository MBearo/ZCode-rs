//! 目标平台与产物的头（remote-exec.md §28.2、§28.4）：`uname` 的两个字段 → 平台、平台 → 包 id / target、ELF 与 Mach-O
//! （瘦包与 universal）的识别。

use crate::artifact::Platform;

/// F107（§28.2）：`uname -s` / `uname -m` → 平台；Windows 与认不出来的架构给 None。
#[test]
fn the_remote_system_and_machine_name_pick_a_platform() {
    for (os, machine, platform) in [
        ("Linux", "x86_64", Platform::LinuxX86_64),
        ("Linux", "amd64", Platform::LinuxX86_64),
        ("Linux", "aarch64", Platform::LinuxAarch64),
        ("Linux", "arm64", Platform::LinuxAarch64),
        ("Darwin", "arm64", Platform::MacosAarch64),
        ("Darwin", "aarch64", Platform::MacosAarch64),
        ("Darwin", "x86_64", Platform::MacosX86_64),
    ] {
        assert_eq!(Platform::of(os, machine), Some(platform), "{os} {machine}");
    }
    for (os, machine) in [
        ("MINGW64_NT-10.0-22631", "x86_64"),
        ("CYGWIN_NT-10.0", "x86_64"),
        ("MSYS_NT-10.0", "x86_64"),
        ("FreeBSD", "amd64"),
        ("Linux", "riscv64"),
        ("Darwin", "i386"),
        ("", ""),
    ] {
        assert_eq!(Platform::of(os, machine), None, "{os} {machine}");
    }
}

/// F107（§28.2、§28.8）：每个平台的包 id、下载与缓存的 target、静态产物的目录。Intel Mac 没有发布的包。
#[test]
fn every_platform_has_a_package_id_and_a_target() {
    for (platform, id, triple, statically) in [
        (
            Platform::LinuxX86_64,
            "linux-x64-gnu",
            "x86_64-unknown-linux-gnu",
            Some("x86_64-unknown-linux-musl"),
        ),
        (
            Platform::LinuxAarch64,
            "linux-arm64-gnu",
            "aarch64-unknown-linux-gnu",
            Some("aarch64-unknown-linux-musl"),
        ),
        (
            Platform::MacosAarch64,
            "darwin-arm64",
            "aarch64-apple-darwin",
            None,
        ),
        (
            Platform::MacosX86_64,
            "darwin-x64",
            "x86_64-apple-darwin",
            None,
        ),
    ] {
        assert_eq!(platform.package_id(), id);
        assert_eq!(platform.triple(), triple);
        assert_eq!(platform.static_triple(), statically);
        // 报“没有产物”时写静态目录，macOS 没有静态目录就写下载的 target。
        assert_eq!(platform.missing_triple(), statically.unwrap_or(triple));
        assert_eq!(platform.is_macos(), platform.triple().contains("darwin"));
    }
    // 只有 Intel Mac 没有发布的包（§28.2）。
    assert!(!Platform::MacosX86_64.published());
    for platform in [
        Platform::LinuxX86_64,
        Platform::LinuxAarch64,
        Platform::MacosAarch64,
    ] {
        assert!(platform.published(), "{platform:?}");
    }
    assert_eq!(
        Platform::LINUX,
        [Platform::LinuxX86_64, Platform::LinuxAarch64]
    );
}

/// 64 位小端的 ELF 头（`e_machine` 在偏移 18）。
fn elf(machine: u16) -> Vec<u8> {
    let mut head = vec![0u8; 64];
    head[..6].copy_from_slice(b"\x7fELF\x02\x01");
    head[16] = 2;
    head[18..20].copy_from_slice(&machine.to_le_bytes());
    head
}

/// 64 位的瘦 Mach-O 头（`0xfeedfacf` + `cputype`，都是小端）。
fn mach(cpu: u32) -> Vec<u8> {
    let mut head = vec![0u8; 64];
    head[..4].copy_from_slice(&0xfeed_facfu32.to_le_bytes());
    head[4..8].copy_from_slice(&cpu.to_le_bytes());
    head
}

/// universal 的 Mach-O 头（大端：魔数、片数，然后每片 20 / 32 字节，开头是 `cputype`）。
fn universal(magic: u32, slices: &[u32]) -> Vec<u8> {
    let entry = if magic == 0xcafe_babf { 32 } else { 20 };
    let mut head = vec![0u8; 8 + entry * slices.len()];
    head[..4].copy_from_slice(&magic.to_be_bytes());
    head[4..8].copy_from_slice(&(slices.len() as u32).to_be_bytes());
    for (index, cpu) in slices.iter().enumerate() {
        let at = 8 + entry * index;
        head[at..at + 4].copy_from_slice(&cpu.to_be_bytes());
    }
    head
}

const ARM64: u32 = 0x0100_000c;
const X86_64: u32 = 0x0100_0007;

/// F107（§28.4）：产物的头认 ELF 与 Mach-O；架构不符、32 位、截断与别的格式都不认。
#[test]
fn the_image_header_says_whether_the_build_runs_on_the_platform() {
    let only = |head: &[u8], expected: &[Platform]| {
        for platform in [
            Platform::LinuxX86_64,
            Platform::LinuxAarch64,
            Platform::MacosAarch64,
            Platform::MacosX86_64,
        ] {
            assert_eq!(
                platform.runs_image(head),
                expected.contains(&platform),
                "{platform:?}"
            );
        }
    };
    // ELF：两种 Linux，只对得上自己。
    only(&elf(183), &[Platform::LinuxAarch64]);
    only(&elf(62), &[Platform::LinuxX86_64]);
    only(&elf(243), &[]);
    only(b"\x7fELF", &[]);
    let mut thirty_two = elf(62);
    thirty_two[4] = 1;
    only(&thirty_two, &[]);

    // Mach-O 的瘦包：只对得上那一种 macOS。
    only(&mach(ARM64), &[Platform::MacosAarch64]);
    only(&mach(X86_64), &[Platform::MacosX86_64]);
    only(&mach(0x0100_00ff), &[]);
    only(&mach(ARM64)[..6], &[]);
    // 32 位的 Mach-O（`0xfeedface`）不认：执行器只有 64 位。
    let mut thin32 = mach(ARM64);
    thin32[..4].copy_from_slice(&0xfeed_faceu32.to_le_bytes());
    only(&thin32, &[]);

    // universal：每一片都算（整包上传，内核自己挑片）。
    only(
        &universal(0xcafe_babe, &[X86_64, ARM64]),
        &[Platform::MacosAarch64, Platform::MacosX86_64],
    );
    only(&universal(0xcafe_babf, &[ARM64]), &[Platform::MacosAarch64]);
    only(&universal(0xcafe_babe, &[0x0000_0012]), &[]);
    only(&universal(0xcafe_babe, &[]), &[]);
    // 片表不完整（头只读到一部分）时不猜。
    only(&universal(0xcafe_babe, &[ARM64])[..12], &[]);
    // 别的格式的 `0xcafebabe`（片数大得离谱）不认。
    let mut java = vec![0u8; 64];
    java[..4].copy_from_slice(&0xcafe_babeu32.to_be_bytes());
    java[4..8].copy_from_slice(&0x0000_0037u32.to_be_bytes());
    only(&java, &[]);
}

/// F107（§28.4）：`locate` 按头挑文件——Linux 的 ELF 不会被当成 macOS 的产物，反之也不会。
#[cfg(unix)]
#[tokio::test]
async fn locate_picks_the_file_whose_header_matches_the_platform() {
    #![allow(clippy::disallowed_methods, reason = "测试直接准备文件")]
    let dir = tempfile::tempdir().unwrap();
    let write = |name: &str, bytes: &[u8]| {
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    };
    let linux = write("linux", &elf(183));
    let darwin = write("darwin", &mach(ARM64));
    let empty = write("empty", b"");
    let candidates = vec![
        dir.path().join("missing"),
        empty,
        linux.clone(),
        darwin.clone(),
    ];
    assert_eq!(
        crate::artifact::locate(&candidates, Platform::LinuxAarch64).await,
        Some(linux)
    );
    assert_eq!(
        crate::artifact::locate(&candidates, Platform::MacosAarch64).await,
        Some(darwin)
    );
    assert_eq!(
        crate::artifact::locate(&candidates, Platform::MacosX86_64).await,
        None
    );
}
