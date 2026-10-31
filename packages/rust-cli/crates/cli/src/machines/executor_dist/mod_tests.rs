//! 执行器的 npm 来源（F100，docs/specs/rust-cli/remote-exec.md §26.4）：对着本地假注册表——下载、校验、缓存；integrity、
//! build.json 的 sha256 与 target、ELF 头不对时不留东西；404 与缺版本说明原因；缓存命中不再下载；两台机器同时要只下载
//! 一次；release 构建总是官方注册表。
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "测试直接准备文件系统夹具与本地 HTTP 服务"
)]

use std::path::Path;
use std::sync::Arc;

use rust_cli_base::hosts::NPM_REGISTRY;
use rust_cli_net::{Egress, NetworkPolicy};
use rust_cli_paths::ExecutorPaths;
use rust_cli_ssh_connect::{ArtifactSource, DownloadError, Platform};

use super::fake_npm_tests::{FakeNpm, build_json, image, package, sha256_hex, sri};
use super::{NpmExecutors, registry};
use crate::options::TestDeclarations;

const X64: &str = "@mbearo/zcode-linux-x64-gnu";
const TARGET: &str = "x86_64-unknown-linux-gnu";

struct Setup {
    npm: FakeNpm,
    home: tempfile::TempDir,
    source: NpmExecutors,
}

async fn setup() -> Setup {
    let npm = FakeNpm::start();
    let home = tempfile::tempdir().unwrap();
    let egress = Arc::new(Egress::new(NetworkPolicy::default()).await.unwrap());
    let source = NpmExecutors::new(egress, npm.url.clone(), executor(home.path()));
    Setup { npm, home, source }
}

fn executor(home: &Path) -> ExecutorPaths {
    rust_cli_paths::StoragePaths::resolve(&Default::default(), home, home).executor()
}

fn cached_path(home: &Path, version: &str) -> std::path::PathBuf {
    executor(home)
        .artifact(version, TARGET)
        .as_path()
        .to_path_buf()
}

/// 一个正确的 x86_64 包。
fn good(version: &str) -> (Vec<u8>, Vec<u8>) {
    let binary = image(Platform::LinuxX86_64);
    let build = build_json(version, TARGET, &sha256_hex(&binary));
    (binary.clone(), package(&binary, &build))
}

/// Unix 的权限位（Windows 上没有，不查）。
#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// 失败之后缓存的目录里什么都没有（没有半个文件，也没有临时文件）。
fn nothing_cached(home: &Path, version: &str) {
    let dir = cached_path(home, version).parent().unwrap().to_path_buf();
    let left: Vec<_> = std::fs::read_dir(&dir)
        .map(|entries| entries.map(|entry| entry.unwrap().file_name()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "{left:?}");
}

#[tokio::test]
async fn a_package_is_downloaded_verified_and_cached() {
    let setup = setup().await;
    let (binary, tarball) = good("0.2.0");
    let packument = setup.npm.publish(X64, "0.2.0", &tarball, None);
    assert_eq!(packument, "/@mbearo%2fzcode-linux-x64-gnu");
    assert_eq!(
        setup.source.cached("0.2.0", Platform::LinuxX86_64).await,
        None
    );
    let path = setup
        .source
        .download("0.2.0", Platform::LinuxX86_64)
        .await
        .unwrap();
    assert_eq!(path, cached_path(setup.home.path(), "0.2.0"));
    assert_eq!(std::fs::read(&path).unwrap(), binary);
    #[cfg(unix)]
    {
        assert_eq!(mode(&path), 0o755);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
    }
    let seen = setup.npm.seen();
    assert_eq!(
        seen,
        [
            packument,
            "/@mbearo/zcode-linux-x64-gnu/-/zcode-linux-x64-gnu-0.2.0.tgz".to_owned()
        ]
    );
    // 缓存命中：来源报告它，再要也不联网。
    assert_eq!(
        setup.source.cached("0.2.0", Platform::LinuxX86_64).await,
        Some(path.clone())
    );
    assert_eq!(
        setup
            .source
            .download("0.2.0", Platform::LinuxX86_64)
            .await
            .unwrap(),
        path
    );
    assert_eq!(setup.npm.seen().len(), 2, "no second download");
    // 另一种架构是另一个包、另一个缓存目录。
    assert_eq!(
        setup.source.cached("0.2.0", Platform::LinuxAarch64).await,
        None
    );
}

#[tokio::test]
async fn a_wrong_integrity_is_refused_and_nothing_is_cached() {
    let setup = setup().await;
    let (_, tarball) = good("0.2.0");
    setup
        .npm
        .publish(X64, "0.2.0", &tarball, Some(sri(b"something else")));
    let error = setup
        .source
        .download("0.2.0", Platform::LinuxX86_64)
        .await
        .unwrap_err();
    assert_eq!(
        error,
        DownloadError::Failed(
            "@mbearo/zcode-linux-x64-gnu 0.2.0: the download does not match the registry's sha512 integrity"
                .to_owned()
        )
    );
    nothing_cached(setup.home.path(), "0.2.0");
    // 只有 sha1 的 integrity 也不认。
    let setup = self::setup().await;
    setup
        .npm
        .publish(X64, "0.2.0", &tarball, Some("sha1-AAAA".to_owned()));
    assert!(
        setup
            .source
            .download("0.2.0", Platform::LinuxX86_64)
            .await
            .unwrap_err()
            .reason()
            .contains("sha512 integrity")
    );
}

#[tokio::test]
async fn a_wrong_build_record_or_binary_is_refused() {
    let binary = image(Platform::LinuxX86_64);
    let cases = [
        (
            build_json("0.2.0", TARGET, &sha256_hex(b"another binary")),
            binary.clone(),
            "build.json's sha256 does not match bin/zcode-rs",
        ),
        (
            build_json("0.2.0", "aarch64-unknown-linux-gnu", &sha256_hex(&binary)),
            binary.clone(),
            "build.json's target is \"aarch64-unknown-linux-gnu\", expected x86_64-unknown-linux-gnu",
        ),
        (
            build_json("0.1.9", TARGET, &sha256_hex(&binary)),
            binary.clone(),
            "build.json's version is \"0.1.9\", expected 0.2.0",
        ),
        (
            build_json("0.2.0", TARGET, &sha256_hex(&image(Platform::LinuxAarch64))),
            image(Platform::LinuxAarch64),
            "bin/zcode-rs is not a x86_64-unknown-linux-gnu executable",
        ),
    ];
    for (build, binary, why) in cases {
        let setup = setup().await;
        setup
            .npm
            .publish(X64, "0.2.0", &package(&binary, &build), None);
        let error = setup
            .source
            .download("0.2.0", Platform::LinuxX86_64)
            .await
            .unwrap_err();
        assert_eq!(error, DownloadError::Failed(format!("{X64} 0.2.0: {why}")));
        nothing_cached(setup.home.path(), "0.2.0");
    }
}

#[tokio::test]
async fn a_missing_package_or_version_says_why() {
    let setup = setup().await;
    let error = setup
        .source
        .download("0.2.0", Platform::LinuxX86_64)
        .await
        .unwrap_err();
    // F103（§27.3）：包从来没发过：不在注册表里，也没有 latest 可退。
    let DownloadError::Unpublished { reason, latest } = error else {
        panic!("{error:?}");
    };
    assert!(
        reason.ends_with("/@mbearo%2fzcode-linux-x64-gnu: HTTP 404"),
        "{reason}"
    );
    assert_eq!(latest, None);
    let (_, tarball) = good("0.1.9");
    setup.npm.publish(X64, "0.1.9", &tarball, None);
    let error = setup
        .source
        .download("0.2.0", Platform::LinuxX86_64)
        .await
        .unwrap_err();
    assert_eq!(
        error,
        DownloadError::Unpublished {
            reason: "npm package @mbearo/zcode-linux-x64-gnu version 0.2.0 is missing".to_owned(),
            latest: Some("0.1.9".to_owned()),
        }
    );
    nothing_cached(setup.home.path(), "0.2.0");
}

/// 两台机器同时第一次连、要同一个（版本，target）：只下载一次，两边拿到同一个结果。
#[tokio::test]
async fn two_machines_at_once_download_once() {
    let setup = setup().await;
    let (binary, tarball) = good("0.2.0");
    setup.npm.publish(X64, "0.2.0", &tarball, None);
    let (a, b) = tokio::join!(
        setup.source.download("0.2.0", Platform::LinuxX86_64),
        setup.source.download("0.2.0", Platform::LinuxX86_64)
    );
    assert_eq!(a.clone().unwrap(), b.unwrap());
    assert_eq!(std::fs::read(a.unwrap()).unwrap(), binary);
    assert_eq!(setup.npm.seen().len(), 2, "{:?}", setup.npm.seen());
    // 失败也一起拿到同一个原因，完成后再要会重新下载。
    let setup = self::setup().await;
    let (a, b) = tokio::join!(
        setup.source.download("0.2.0", Platform::LinuxX86_64),
        setup.source.download("0.2.0", Platform::LinuxX86_64)
    );
    assert_eq!(a.clone().unwrap_err(), b.unwrap_err());
    assert_eq!(setup.npm.seen().len(), 1);
    let _ = setup.source.download("0.2.0", Platform::LinuxX86_64).await;
    assert_eq!(setup.npm.seen().len(), 2);
}

#[test]
fn release_builds_always_use_the_official_registry() {
    let fake = Some("http://127.0.0.1:9".to_owned());
    assert_eq!(
        registry(TestDeclarations::Ignored, fake.clone()).as_deref(),
        Some(NPM_REGISTRY)
    );
    assert_eq!(
        registry(TestDeclarations::Ignored, None).as_deref(),
        Some(NPM_REGISTRY)
    );
    assert_eq!(registry(TestDeclarations::Accepted, fake.clone()), fake);
    assert_eq!(registry(TestDeclarations::Accepted, None), None);
}

#[test]
fn each_architecture_has_its_platform_package() {
    assert_eq!(super::package::package_name(Platform::LinuxX86_64), X64);
    assert_eq!(
        super::package::package_name(Platform::LinuxAarch64),
        "@mbearo/zcode-linux-arm64-gnu"
    );
    // F107（remote-exec.md §28.2）：macOS 的包；`darwin-x64` 没有发布，只用来写错误。
    assert_eq!(
        super::package::package_name(Platform::MacosAarch64),
        "@mbearo/zcode-darwin-arm64"
    );
    assert_eq!(
        super::package::package_name(Platform::MacosX86_64),
        "@mbearo/zcode-darwin-x64"
    );
}

/// F107（remote-exec.md §28.2、§28.8）：macOS 的远端下 `darwin-arm64` 的包，缓存在 `aarch64-apple-darwin/`（与 Linux 的
/// aarch64 互不覆盖）；包里的产物是 Mach-O（ELF 的不收）。Intel Mac 一个请求都不发。
#[tokio::test]
async fn a_macos_package_is_downloaded_and_cached_under_its_own_target() {
    let setup = setup().await;
    const DARWIN: &str = "@mbearo/zcode-darwin-arm64";
    const DARWIN_TARGET: &str = "aarch64-apple-darwin";
    let binary = image(Platform::MacosAarch64);
    let build = build_json("0.2.0", DARWIN_TARGET, &sha256_hex(&binary));
    let packument = setup
        .npm
        .publish(DARWIN, "0.2.0", &package(&binary, &build), None);
    assert_eq!(packument, "/@mbearo%2fzcode-darwin-arm64");
    let path = setup
        .source
        .download("0.2.0", Platform::MacosAarch64)
        .await
        .unwrap();
    assert_eq!(
        path,
        executor(setup.home.path())
            .artifact("0.2.0", DARWIN_TARGET)
            .as_path()
    );
    assert_eq!(std::fs::read(&path).unwrap(), binary);
    // Linux 的 aarch64 缓存是另一个目录（同一个版本、同一个架构，不互相覆盖）。
    assert_eq!(
        setup.source.cached("0.2.0", Platform::LinuxAarch64).await,
        None
    );
    assert_eq!(
        setup.source.cached("0.2.0", Platform::MacosAarch64).await,
        Some(path)
    );

    // 包里放 Linux 的产物：头不对，不收（另一套夹具：假注册表每个包只记一个版本）。
    let other = self::setup().await;
    let wrong = image(Platform::LinuxAarch64);
    let build = build_json("0.2.0", DARWIN_TARGET, &sha256_hex(&wrong));
    other
        .npm
        .publish(DARWIN, "0.2.0", &package(&wrong, &build), None);
    let error = other
        .source
        .download("0.2.0", Platform::MacosAarch64)
        .await
        .unwrap_err();
    assert!(
        error
            .reason()
            .contains("bin/zcode-rs is not a aarch64-apple-darwin executable"),
        "{error:?}"
    );

    // Intel Mac：没有发布的包，不联网（部署器本来也不会走到这里）。
    let before = setup.npm.seen().len();
    let error = setup
        .source
        .download("0.2.0", Platform::MacosX86_64)
        .await
        .unwrap_err();
    assert_eq!(
        error,
        DownloadError::Failed(
            "there is no zcode executor package for x86_64-apple-darwin".to_owned()
        )
    );
    assert_eq!(
        setup.npm.seen().len(),
        before,
        "no request for an Intel Mac"
    );
    assert_eq!(
        setup.source.cached("0.2.0", Platform::MacosX86_64).await,
        None
    );
}
