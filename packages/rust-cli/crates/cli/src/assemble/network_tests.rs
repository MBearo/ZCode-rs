//! 网络出口的来源（docs/specs/rust-cli/config.md §2.4）：两套变量的优先级与格式、CA 文件的读法与出错时的行为。
use super::*;

fn env(vars: &[(&str, &str)]) -> EnvSnapshot {
    vars.iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect()
}

fn proxy_of(vars: &[(&str, &str)]) -> Option<String> {
    proxy(&env(vars)).unwrap()
}

#[test]
fn the_zcode_proxy_wins_and_takes_the_node_shape() {
    // 去空白，没有 scheme 时补 `http://`。
    assert_eq!(
        proxy_of(&[("ZCODE_HTTP_PROXY", " 127.0.0.1:8080 ")]),
        Some("http://127.0.0.1:8080/".to_owned())
    );
    assert_eq!(
        proxy_of(&[("ZCODE_HTTP_PROXY", "socks5://proxy.example:1080")]),
        Some("socks5://proxy.example:1080".to_owned())
    );
    // 有效时不读标准变量，它们格式不对也不报错。
    assert_eq!(
        proxy_of(&[
            ("HTTPS_PROXY", "proxy.example.com:8080"),
            ("ZCODE_HTTP_PROXY", "http://zcode.example:3128"),
        ]),
        Some("http://zcode.example:3128/".to_owned())
    );
}

#[test]
fn an_unusable_zcode_proxy_counts_as_unset() {
    for value in ["", "   ", "http://exa mple:1"] {
        assert_eq!(
            proxy_of(&[
                ("ZCODE_HTTP_PROXY", value),
                ("HTTPS_PROXY", "http://standard.example:8080"),
            ]),
            Some("http://standard.example:8080/".to_owned()),
            "{value:?}"
        );
        assert_eq!(proxy_of(&[("ZCODE_HTTP_PROXY", value)]), None, "{value:?}");
    }
}

#[test]
fn standard_proxies_follow_the_fixed_order() {
    let all = [
        ("HTTP_PROXY", "http://d.example:1"),
        ("http_proxy", "http://c.example:1"),
        ("HTTPS_PROXY", "http://b.example:1"),
        ("https_proxy", "http://a.example:1"),
    ];
    assert_eq!(proxy_of(&all), Some("http://a.example:1/".to_owned()));
    assert_eq!(proxy_of(&all[..3]), Some("http://b.example:1/".to_owned()));
    assert_eq!(proxy_of(&all[..2]), Some("http://c.example:1/".to_owned()));
    assert_eq!(proxy_of(&all[..1]), Some("http://d.example:1/".to_owned()));
    // 空值跳过（取第一个“真值”）。
    assert_eq!(
        proxy_of(&[("https_proxy", ""), ("HTTPS_PROXY", "http://b.example:1")]),
        Some("http://b.example:1/".to_owned())
    );
    assert_eq!(proxy_of(&[]), None);
}

#[test]
fn an_invalid_standard_proxy_is_a_startup_error() {
    let error = proxy(&env(&[("HTTPS_PROXY", "proxy.example.com:8080")])).unwrap_err();
    assert_eq!(
        error.to_string(),
        "Invalid proxy URL in HTTPS_PROXY: \"proxy.example.com:8080\" cannot be parsed as a URL.\n\
         Proxy settings must be a complete URL including the scheme, e.g. \"http://proxy.example.com:8080\".\n\
         Fix or unset HTTPS_PROXY and restart ZCode."
    );
    // 变量名是取到值的那个；值去掉控制字符。前面的变量是空的也不算。
    let error = proxy(&env(&[
        ("https_proxy", ""),
        ("HTTP_PROXY", "local\u{7}host:8080"),
    ]))
    .unwrap_err();
    assert!(
        error
            .to_string()
            .starts_with("Invalid proxy URL in HTTP_PROXY: \"localhost:8080\" cannot"),
        "{error}"
    );
    // 标准变量不补 scheme，也不去空白；没有主机的 URL 也不行。
    for value in ["127.0.0.1:8080", "http://", "socks5://", "not a url"] {
        assert!(proxy(&env(&[("http_proxy", value)])).is_err(), "{value:?}");
    }
}

#[test]
fn no_proxy_prefers_zcode_then_merges_the_standard_pair() {
    let of = |vars: &[(&str, &str)]| no_proxy(&env(vars));
    assert_eq!(
        of(&[
            ("ZCODE_NO_PROXY", " a.test,b.test "),
            ("NO_PROXY", "c.test")
        ]),
        NoProxy::Node("a.test,b.test".to_owned())
    );
    // 去空白后为空等于没有。
    assert_eq!(
        of(&[("ZCODE_NO_PROXY", "  "), ("NO_PROXY", "c.test")]),
        NoProxy::Standard("c.test".to_owned())
    );
    // 合并：任一个是 `*` 时为 `*`；都非空且不同时拼起来；否则取非空的那个。
    assert_eq!(
        of(&[("no_proxy", "a.test"), ("NO_PROXY", "b.test")]),
        NoProxy::Standard("a.test,b.test".to_owned())
    );
    assert_eq!(
        of(&[("no_proxy", "a.test"), ("NO_PROXY", "a.test")]),
        NoProxy::Standard("a.test".to_owned())
    );
    assert_eq!(
        of(&[("no_proxy", "a.test"), ("NO_PROXY", "*")]),
        NoProxy::Standard("*".to_owned())
    );
    assert_eq!(
        of(&[("no_proxy", ""), ("NO_PROXY", "b.test")]),
        NoProxy::Standard("b.test".to_owned())
    );
    assert_eq!(of(&[("NO_PROXY", "")]), NoProxy::None);
    assert_eq!(of(&[]), NoProxy::None);
}

#[test]
fn ca_sources_prefer_the_agent_file() {
    let of = |vars: &[(&str, &str)]| ca_source(&env(vars));
    assert_eq!(
        of(&[
            ("NODE_EXTRA_CA_CERTS", "/x.pem"),
            ("ZCODE_AGENT_CA_CERT", " ca.pem ")
        ]),
        Some(CaSource::Agent("ca.pem".to_owned()))
    );
    assert_eq!(
        of(&[
            ("NODE_EXTRA_CA_CERTS", "/x.pem"),
            ("ZCODE_AGENT_CA_CERT", "  ")
        ]),
        Some(CaSource::Extra("/x.pem".to_owned()))
    );
    assert_eq!(of(&[("NODE_EXTRA_CA_CERTS", "")]), None);
    assert_eq!(of(&[]), None);
}

fn certificate() -> String {
    rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])
        .unwrap()
        .cert
        .pem()
}

/// 在临时目录里写文件，按 `vars` 算出口策略（cwd 是临时目录）。
#[expect(clippy::disallowed_methods, reason = "测试在临时目录里准备 CA 文件")]
async fn network_in(files: &[(&str, String)], vars: &[(&str, &str)]) -> Result<Network> {
    let dir = tempfile::tempdir().unwrap();
    for (name, text) in files {
        std::fs::write(dir.path().join(name), text).unwrap();
    }
    network(&env(vars), dir.path()).await
}

#[tokio::test]
async fn the_agent_file_replaces_the_roots() {
    let files = [
        ("ca.pem", certificate()),
        ("junk.pem", "not a certificate\n".to_owned()),
    ];
    // 相对路径相对 cwd。
    let network = network_in(&files, &[("ZCODE_AGENT_CA_CERT", "ca.pem")])
        .await
        .unwrap();
    assert!(matches!(&network.policy.roots, TrustRoots::Only(bundle) if bundle.len() == 1));
    assert!(network.warnings.is_empty());
    // 没有可用证书：照常，HTTPS 在证书校验时失败（Node 相同），不写 stderr。
    let network = network_in(&files, &[("ZCODE_AGENT_CA_CERT", "junk.pem")])
        .await
        .unwrap();
    assert!(matches!(&network.policy.roots, TrustRoots::Only(bundle) if bundle.is_empty()));
    assert!(network.warnings.is_empty());
    // 读不出：报错（config.md D14）。
    let error = network_in(&files, &[("ZCODE_AGENT_CA_CERT", "missing.pem")])
        .await
        .err()
        .unwrap();
    assert_eq!(
        error.to_string(),
        "ZCODE_AGENT_CA_CERT: cannot read missing.pem: No such file or directory"
    );
}

#[tokio::test]
async fn an_unusable_extra_file_is_a_warning() {
    let broken = "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n";
    let files = [
        ("ca.pem", format!("text before\n{}", certificate())),
        ("junk.pem", "not a certificate\n".to_owned()),
        ("half.pem", format!("{}{broken}", certificate())),
    ];
    let network = network_in(&files, &[("NODE_EXTRA_CA_CERTS", "ca.pem")])
        .await
        .unwrap();
    assert!(matches!(&network.policy.roots, TrustRoots::Extra(bundle) if bundle.len() == 1));
    assert!(network.warnings.is_empty());
    let cases = [
        (
            "missing.pem",
            "warn: ignoring extra certs from missing.pem, load failed: No such file or directory\n",
        ),
        (
            "junk.pem",
            "warn: ignoring extra certs from junk.pem, load failed: no certificate\n",
        ),
    ];
    for (file, warning) in cases {
        let network = network_in(&files, &[("NODE_EXTRA_CA_CERTS", file)])
            .await
            .unwrap();
        assert_eq!(network.policy.roots, TrustRoots::Platform, "{file}");
        assert_eq!(network.warnings, vec![warning.to_owned()]);
    }
    // 坏块之前的证书照用，同时写警告。
    let network = network_in(&files, &[("NODE_EXTRA_CA_CERTS", "half.pem")])
        .await
        .unwrap();
    assert!(matches!(&network.policy.roots, TrustRoots::Extra(bundle) if bundle.len() == 1));
    assert_eq!(
        network.warnings,
        vec!["warn: ignoring extra certs from half.pem, load failed: invalid certificate\n"]
    );
}

#[tokio::test]
async fn the_policy_carries_all_three_parts() {
    let network = network_in(
        &[],
        &[
            ("HTTPS_PROXY", "http://proxy.example:3128"),
            ("NO_PROXY", "localhost"),
        ],
    )
    .await
    .unwrap();
    assert_eq!(
        network.policy.proxy.as_deref(),
        Some("http://proxy.example:3128/")
    );
    assert_eq!(
        network.policy.no_proxy,
        NoProxy::Standard("localhost".to_owned())
    );
    assert_eq!(network.policy.roots, TrustRoots::Platform);
    let error = network_in(&[], &[("HTTPS_PROXY", "proxy.example.com:8080")])
        .await
        .err()
        .unwrap();
    assert!(error.downcast_ref::<InvalidProxy>().is_some(), "{error}");
}
