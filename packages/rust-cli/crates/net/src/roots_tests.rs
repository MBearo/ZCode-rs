//! PEM 的读法（docs/specs/rust-cli/config.md §2.4）：与 Node 的 `ca`（OpenSSL）相同，2026-10-19 在 Node 24 上
//! 实测过这几种组合。
use super::*;

fn certificate() -> String {
    rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])
        .unwrap()
        .cert
        .pem()
}

const BROKEN: &str = "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n";

#[test]
fn certificates_are_read_in_order() {
    let one = CaBundle::from_pem(certificate().as_bytes());
    assert_eq!((one.len(), one.problem()), (1, None));
    let two = CaBundle::from_pem(format!("{}{}", certificate(), certificate()).as_bytes());
    assert_eq!((two.len(), two.problem()), (2, None));
}

#[test]
fn text_and_other_blocks_are_skipped() {
    let key = rcgen::KeyPair::generate().unwrap().serialize_pem();
    let pem = format!("issuer: example\n{key}{}trailing text\n", certificate());
    let bundle = CaBundle::from_pem(pem.as_bytes());
    assert_eq!((bundle.len(), bundle.problem()), (1, None));
}

#[test]
fn reading_stops_at_a_broken_certificate() {
    let after =
        CaBundle::from_pem(format!("{}{BROKEN}{}", certificate(), certificate()).as_bytes());
    assert_eq!(
        (after.len(), after.problem()),
        (1, Some("invalid certificate"))
    );
    let before = CaBundle::from_pem(format!("{BROKEN}{}", certificate()).as_bytes());
    assert_eq!(
        (before.len(), before.problem()),
        (0, Some("invalid certificate"))
    );
    // 块没有结束行。
    let open = CaBundle::from_pem(b"-----BEGIN CERTIFICATE-----\nAAAA\n");
    assert_eq!(
        (open.len(), open.problem()),
        (0, Some("invalid certificate"))
    );
}

#[test]
fn no_certificate_at_all() {
    let bundle = CaBundle::from_pem(b"not a certificate\n");
    assert_eq!(
        (bundle.len(), bundle.problem()),
        (0, Some("no certificate"))
    );
    assert!(bundle.is_empty());
}

/// 证书 DER 里的一段 ASCII（主题名里的 CN 是明文）。
fn mentions(der: &[u8], text: &str) -> bool {
    der.windows(text.len())
        .any(|window| window == text.as_bytes())
}

/// 内置的根证书（工作线 F44，config.md §2.4）：Mozilla 的列表，每张都能作为信任锚。
#[test]
fn the_bundled_roots_are_the_mozilla_list() {
    let roots = bundled();
    assert!(roots.len() > 100, "{}", roots.len());
    let mut store = rustls::RootCertStore::empty();
    let (added, ignored) = store.add_parsable_certificates(roots.iter().cloned());
    assert_eq!((added, ignored), (roots.len(), 0));
    for name in ["ISRG Root X1", "DigiCert Global Root G2"] {
        assert!(
            roots.iter().any(|der| mentions(der, name)),
            "{name} is not bundled"
        );
    }
}

/// 合并规则（缺省 `bundled,system`，`NODE_EXTRA_CA_CERTS` 追加；Node 的 `ca` 替换全部）。
/// 缺陷修复（F44）：以前 `Platform` / `Extra` 不带内置的，系统根证书为空的镜像里只剩 `NODE_EXTRA_CA_CERTS`。
#[test]
fn anchors_add_the_bundled_roots_unless_replaced() {
    let bundled = bundled().len();
    let Anchors::Merge(platform) = TrustRoots::Platform.anchors() else {
        panic!("platform roots replace the system store");
    };
    assert_eq!(platform.len(), bundled);
    assert!(platform.iter().any(|der| mentions(der, "ISRG Root X1")));

    let extra = CaBundle::from_pem(certificate().as_bytes());
    let Anchors::Merge(merged) = TrustRoots::Extra(extra.clone()).anchors() else {
        panic!("NODE_EXTRA_CA_CERTS replaces the system store");
    };
    assert_eq!(merged.len(), bundled + 1);
    assert_eq!(merged.last(), extra.certificates.first());

    let Anchors::Only(only) = TrustRoots::Only(extra.clone()).anchors() else {
        panic!("ZCODE_AGENT_CA_CERT keeps the system store");
    };
    assert_eq!(only, extra.certificates);
    let Anchors::Only(none) = TrustRoots::Only(CaBundle::default()).anchors() else {
        panic!("an empty ZCODE_AGENT_CA_CERT keeps the system store");
    };
    assert!(none.is_empty());
}
