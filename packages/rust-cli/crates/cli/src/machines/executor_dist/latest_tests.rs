//! F103（docs/specs/rust-cli/remote-exec.md §27.3、§27.4）：本机的版本不在注册表里时，下载来源报“不在注册表里”并带上
//! 注册表的 latest；别的失败不带。latest 按它自己的版本缓存、单飞：两台机器同时退到 latest 只下载一次。对着本地假注册表，
//! 不连任何真实服务。
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "测试直接准备文件系统夹具与本地 HTTP 服务"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rust_cli_net::{Egress, NetworkPolicy};
use rust_cli_paths::ExecutorPaths;
use rust_cli_ssh_connect::{ArtifactSource, DownloadError, Platform};

use super::NpmExecutors;
use super::fake_npm_tests::{FakeNpm, build_json, image, package, sha256_hex, sri};

const X64: &str = "@mbearo/zcode-linux-x64-gnu";
const PACKUMENT: &str = "/@mbearo%2fzcode-linux-x64-gnu";
const TARGET: &str = "x86_64-unknown-linux-gnu";

fn executor(home: &Path) -> ExecutorPaths {
    rust_cli_paths::StoragePaths::resolve(&Default::default(), home, home).executor()
}

fn cached_path(home: &Path, version: &str) -> PathBuf {
    executor(home)
        .artifact(version, TARGET)
        .as_path()
        .to_path_buf()
}

async fn source(registry: &str, home: &Path) -> NpmExecutors {
    let egress = Arc::new(Egress::new(NetworkPolicy::default()).await.unwrap());
    NpmExecutors::new(egress, registry.to_owned(), executor(home))
}

/// 这个版本的正确的包（tarball 的字节）。
fn tarball(version: &str) -> Vec<u8> {
    let binary = image(Platform::LinuxX86_64);
    package(&binary, &build_json(version, TARGET, &sha256_hex(&binary)))
}

fn tarball_path(version: &str) -> String {
    format!("/{X64}/-/zcode-linux-x64-gnu-{version}.tgz")
}

/// 发布 packument：`versions` 里的每个版本指向它的 tarball 路径（`with_tarball` 的才放 tarball 的路由），`latest` 写进
/// `dist-tags`。
fn publish(npm: &FakeNpm, versions: &[(&str, bool)], latest: &str) {
    let mut listed = serde_json::Map::new();
    for (version, with_tarball) in versions {
        let bytes = tarball(version);
        listed.insert(
            (*version).to_owned(),
            serde_json::json!({
                "name": X64,
                "version": version,
                "dist": {
                    "tarball": format!("{}{}", npm.url, tarball_path(version)),
                    "integrity": sri(&bytes),
                },
            }),
        );
        if *with_tarball {
            npm.route(&tarball_path(version), 200, bytes);
        }
    }
    let document = serde_json::json!({
        "name": X64,
        "dist-tags": {"latest": latest},
        "versions": listed,
    });
    npm.route(PACKUMENT, 200, document.to_string().into_bytes());
}

fn tarball_requests(npm: &FakeNpm) -> usize {
    npm.seen()
        .iter()
        .filter(|path| path.ends_with(".tgz"))
        .count()
}

#[tokio::test]
async fn a_missing_version_names_the_latest_which_is_cached_by_its_own_version() {
    let npm = FakeNpm::start();
    let home = tempfile::tempdir().unwrap();
    let source = source(&npm.url, home.path()).await;
    publish(&npm, &[("0.1.9", true)], "0.1.9");
    assert_eq!(
        source.download("0.2.0", Platform::LinuxX86_64).await,
        Err(DownloadError::Unpublished {
            reason: "npm package @mbearo/zcode-linux-x64-gnu version 0.2.0 is missing".to_owned(),
            latest: Some("0.1.9".to_owned()),
        })
    );
    assert!(!cached_path(home.path(), "0.2.0").parent().unwrap().exists());
    // 部署器按 latest 的实际版本再要：缓存在 dist/0.1.9/，不在本机版本的目录。
    let path = source
        .download("0.1.9", Platform::LinuxX86_64)
        .await
        .unwrap();
    assert_eq!(path, cached_path(home.path(), "0.1.9"));
    assert_eq!(
        source.cached("0.1.9", Platform::LinuxX86_64).await,
        Some(path)
    );
    assert_eq!(source.cached("0.2.0", Platform::LinuxX86_64).await, None);
}

#[tokio::test]
async fn a_missing_tarball_names_the_latest() {
    let npm = FakeNpm::start();
    let home = tempfile::tempdir().unwrap();
    let source = source(&npm.url, home.path()).await;
    publish(&npm, &[("0.2.0", false), ("0.1.9", true)], "0.1.9");
    let error = source
        .download("0.2.0", Platform::LinuxX86_64)
        .await
        .unwrap_err();
    let DownloadError::Unpublished { reason, latest } = error else {
        panic!("{error:?}");
    };
    assert!(
        reason.ends_with(&format!("{}: HTTP 404", tarball_path("0.2.0"))),
        "{reason}"
    );
    assert_eq!(latest.as_deref(), Some("0.1.9"));
}

#[tokio::test]
async fn other_failures_do_not_offer_the_latest() {
    // 注册表出错（5xx）。
    let npm = FakeNpm::start();
    let home = tempfile::tempdir().unwrap();
    npm.route(PACKUMENT, 503, b"busy".to_vec());
    let error = source(&npm.url, home.path())
        .await
        .download("0.2.0", Platform::LinuxX86_64)
        .await
        .unwrap_err();
    assert_eq!(
        error,
        DownloadError::Failed(format!("GET {}{PACKUMENT}: HTTP 503", npm.url))
    );

    // 连不上（端口上没有服务）。
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    let error = source(&url, home.path())
        .await
        .download("0.2.0", Platform::LinuxX86_64)
        .await
        .unwrap_err();
    assert!(matches!(error, DownloadError::Failed(_)), "{error:?}");

    // integrity 不对；packument 里有这个版本但缺 dist 的字段。
    let npm = FakeNpm::start();
    npm.publish(X64, "0.2.0", &tarball("0.2.0"), Some(sri(b"other")));
    let error = source(&npm.url, home.path())
        .await
        .download("0.2.0", Platform::LinuxX86_64)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, DownloadError::Failed(reason) if reason.contains("sha512 integrity")),
        "{error:?}"
    );
    let npm = FakeNpm::start();
    let document = serde_json::json!({
        "name": X64,
        "dist-tags": {"latest": "0.1.9"},
        "versions": {"0.2.0": {"dist": {"integrity": "sha512-AAAA"}}},
    });
    npm.route(PACKUMENT, 200, document.to_string().into_bytes());
    let error = source(&npm.url, home.path())
        .await
        .download("0.2.0", Platform::LinuxX86_64)
        .await
        .unwrap_err();
    assert_eq!(
        error,
        DownloadError::Failed(
            "npm package @mbearo/zcode-linux-x64-gnu version 0.2.0 is missing dist.tarball"
                .to_owned()
        )
    );
}

/// latest 要进本机缓存与远端的路径：不合规则的不给。
#[tokio::test]
async fn only_a_plain_latest_is_offered() {
    let cases = [
        (" 0.1.9 ", Some("0.1.9")),
        ("0.0.0-placeholder", Some("0.0.0-placeholder")),
        ("1.0.0+build.7", Some("1.0.0+build.7")),
        ("../../escape", None),
        ("0.1..9", None),
        ("v0.1.9", None),
        ("0.1.9/x", None),
        ("", None),
    ];
    for (tag, offered) in cases {
        let npm = FakeNpm::start();
        let home = tempfile::tempdir().unwrap();
        let document = serde_json::json!({
            "name": X64,
            "dist-tags": {"latest": tag},
            "versions": {},
        });
        npm.route(PACKUMENT, 200, document.to_string().into_bytes());
        let error = source(&npm.url, home.path())
            .await
            .download("0.2.0", Platform::LinuxX86_64)
            .await
            .unwrap_err();
        let DownloadError::Unpublished { latest, .. } = error else {
            panic!("{tag:?}: {error:?}");
        };
        assert_eq!(latest.as_deref(), offered, "{tag:?}");
    }
}

/// 两台机器同时第一次连：都等本机版本的同一次（不在注册表里），再都要 latest：tarball 只下载一次，两边拿到同一个文件。
#[tokio::test]
async fn two_machines_falling_back_at_once_download_the_latest_once() {
    let npm = FakeNpm::start();
    let home = tempfile::tempdir().unwrap();
    let source = source(&npm.url, home.path()).await;
    publish(&npm, &[("0.1.9", true)], "0.1.9");
    let fall_back = || async {
        match source.download("0.2.0", Platform::LinuxX86_64).await {
            Err(DownloadError::Unpublished {
                latest: Some(latest),
                ..
            }) => source.download(&latest, Platform::LinuxX86_64).await,
            other => panic!("{other:?}"),
        }
    };
    let (a, b) = tokio::join!(fall_back(), fall_back());
    assert_eq!(a.clone().unwrap(), b.unwrap());
    assert_eq!(a.unwrap(), cached_path(home.path(), "0.1.9"));
    assert_eq!(tarball_requests(&npm), 1, "{:?}", npm.seen());
}
