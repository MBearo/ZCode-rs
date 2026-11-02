use serde_json::json;

use super::super::fake_registry_tests::{FakeRegistry, egress, package};
use super::super::install::Manager;
use super::*;

fn install(platform: Option<&str>) -> Install {
    Install {
        manager: Manager::Npm,
        package: "@scope/zcode".to_owned(),
        platform_package: platform.map(str::to_owned),
    }
}

const ENTRY: &str = "/@scope%2fzcode";
const PLATFORM: &str = "/@scope%2fzcode-darwin-arm64";

#[test]
fn scoped_packages_escape_the_slash() {
    assert_eq!(
        package_url(REGISTRY, "@scope/zcode"),
        format!("{REGISTRY}{ENTRY}")
    );
    assert_eq!(
        package_url("http://127.0.0.1:1/", "zcode"),
        "http://127.0.0.1:1/zcode"
    );
}

#[tokio::test]
async fn the_latest_tag_is_read_and_both_packages_are_checked() {
    let registry = FakeRegistry::start(vec![
        (
            ENTRY.to_owned(),
            200,
            package("@scope/zcode", "0.1.47", &["0.1.46", "0.1.47"]),
        ),
        (
            PLATFORM.to_owned(),
            200,
            package("@scope/zcode-darwin-arm64", "0.1.47", &["0.1.47"]),
        ),
    ]);
    let egress = egress().await;
    let latest = latest(
        &egress,
        &registry.url,
        &install(Some("@scope/zcode-darwin-arm64")),
    )
    .await
    .unwrap();
    assert_eq!(latest, "0.1.47");
    let seen = registry.seen();
    assert_eq!(
        seen.iter().map(|s| s.path.as_str()).collect::<Vec<_>>(),
        [ENTRY, PLATFORM]
    );
    // npm 的缩略元数据。
    assert!(
        seen[0]
            .accept
            .as_deref()
            .is_some_and(|accept| accept.starts_with("application/vnd.npm.install-v1+json")),
        "{seen:?}"
    );
}

#[tokio::test]
async fn without_a_platform_package_only_the_entry_is_checked() {
    let registry = FakeRegistry::start(vec![(
        ENTRY.to_owned(),
        200,
        package("@scope/zcode", "0.1.47", &["0.1.47"]),
    )]);
    let latest = latest(&egress().await, &registry.url, &install(None))
        .await
        .unwrap();
    assert_eq!(latest, "0.1.47");
    assert_eq!(registry.seen().len(), 1);
}

#[tokio::test]
async fn a_version_that_cannot_be_installed_fails_the_check() {
    let cases = [
        // 平台包还没有这个版本（入口先发了）。
        (
            package("@scope/zcode", "0.1.47", &["0.1.47"]),
            Some(package("@scope/zcode-darwin-arm64", "0.1.46", &["0.1.46"])),
            "@scope/zcode-darwin-arm64 version 0.1.47 is missing",
        ),
        // 入口包的 latest 没有 dist。
        (
            json!({"dist-tags": {"latest": "0.1.47"}, "versions": {"0.1.47": {}}}).to_string(),
            None,
            "missing dist metadata",
        ),
        (
            json!({"dist-tags": {"latest": "0.1.47"}, "versions": {"0.1.47": {"dist": {"tarball": "t"}}}})
                .to_string(),
            None,
            "missing dist.integrity",
        ),
        (
            json!({"dist-tags": {"latest": "0.1.47"}, "versions": {"0.1.47": {"dist": {"tarball": "", "integrity": "i"}}}})
                .to_string(),
            None,
            "missing dist.tarball",
        ),
        (
            json!({"versions": {}}).to_string(),
            None,
            "missing latest dist-tag",
        ),
        ("not json".to_owned(), None, "not JSON"),
    ];
    for (entry, platform, expected) in cases {
        let mut routes = vec![(ENTRY.to_owned(), 200, entry)];
        if let Some(platform) = platform.clone() {
            routes.push((PLATFORM.to_owned(), 200, platform));
        }
        let registry = FakeRegistry::start(routes);
        let platform_name = platform.is_some().then_some("@scope/zcode-darwin-arm64");
        let error = latest(&egress().await, &registry.url, &install(platform_name))
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains(expected),
            "{expected}: {error:#}"
        );
    }
}

#[tokio::test]
async fn http_errors_fail_the_check() {
    let registry = FakeRegistry::start(vec![(ENTRY.to_owned(), 500, "{}".to_owned())]);
    let error = latest(&egress().await, &registry.url, &install(None))
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("HTTP 500"), "{error:#}");
    // 没有这个包（404）。
    let missing = FakeRegistry::start(Vec::new());
    let error = latest(&egress().await, &missing.url, &install(None))
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("HTTP 404"), "{error:#}");
}

/// registry 不回响应头：5 s 后按失败处理（修复线 F58，net `timeout::NPM_REGISTRY`）。以前没有
/// 单次超时，后台检查一直挂着。
#[tokio::test]
#[allow(
    clippy::disallowed_methods,
    reason = "测试搭一个只收连接、不回复的本地服务"
)]
async fn a_silent_registry_times_out() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming().flatten() {
            held.push(stream);
        }
    });
    let started = std::time::Instant::now();
    let error = latest(&egress().await, &url, &install(None))
        .await
        .unwrap_err();
    let elapsed = started.elapsed();
    assert!(
        elapsed >= std::time::Duration::from_secs(4)
            && elapsed < std::time::Duration::from_secs(15),
        "{elapsed:?} {error:#}"
    );
}
