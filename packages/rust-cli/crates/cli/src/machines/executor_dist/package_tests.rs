//! npm 平台包的纯计算部分（remote-exec.md §26.4）：integrity 的几种写法、tar 的 ustar 前缀与 pax 的 `path`、截断与缺项。

use rust_cli_ssh_connect::Arch;

use super::super::fake_npm_tests::{build_json, elf, gzip, header, sha256_hex, sri, tar};
use super::{extract, integrity_matches};

#[test]
fn integrity_needs_a_matching_sha512_entry() {
    let bytes = b"tarball bytes";
    assert!(integrity_matches(bytes, &sri(bytes)));
    assert!(integrity_matches(
        bytes,
        &format!("sha1-AAAA {}?opt", sri(bytes))
    ));
    assert!(!integrity_matches(bytes, &sri(b"other")));
    assert!(!integrity_matches(bytes, "sha1-AAAA"));
    assert!(!integrity_matches(bytes, ""));
}

#[test]
fn entries_named_by_a_ustar_prefix_or_a_pax_path_are_found() {
    let binary = elf(Arch::Aarch64);
    let build = build_json("0.2.0", "aarch64-unknown-linux-gnu", &sha256_hex(&binary));
    let pax = b"30 path=package/bin/zcode-rs\n".to_vec();
    let archive = gzip(&tar(&[
        (header("build.json", "package", build.len(), b'0'), build),
        (header("PaxHeader/zcode-rs", "", pax.len(), b'x'), pax),
        (
            header("ignored-short-name", "", binary.len(), b'0'),
            binary.clone(),
        ),
    ]));
    assert_eq!(extract(&archive, "0.2.0", Arch::Aarch64).unwrap(), binary);
}

#[test]
fn a_cut_or_incomplete_package_is_refused() {
    let binary = elf(Arch::X86_64);
    let build = build_json("0.2.0", "x86_64-unknown-linux-gnu", &sha256_hex(&binary));
    let whole = tar(&[
        (
            header("package/build.json", "", build.len(), b'0'),
            build.clone(),
        ),
        (
            header("package/bin/zcode-rs", "", binary.len(), b'0'),
            binary,
        ),
    ]);
    let cut = gzip(&whole[..1500]);
    let error = extract(&cut, "0.2.0", Arch::X86_64).unwrap_err();
    assert!(error.starts_with("cannot read the package"), "{error}");
    let only_build = gzip(&tar(&[(
        header("package/build.json", "", build.len(), b'0'),
        build,
    )]));
    assert_eq!(
        extract(&only_build, "0.2.0", Arch::X86_64).unwrap_err(),
        "the package has no bin/zcode-rs"
    );
    assert!(
        extract(b"not gzip", "0.2.0", Arch::X86_64)
            .unwrap_err()
            .starts_with("cannot read the package")
    );
}
