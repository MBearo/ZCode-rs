//! F103（remote-exec.md §27.2、§27.5）：本机的版本不在注册表里时，先用远端协议相同的候选，再用注册表的 latest——上传到
//! `bin/<latest>/`，改名前试跑核对协议；协议不同时报清楚的错误、远端不留文件；网络错误与校验不对不退到 latest。远端是
//! 假的 `ssh`（`provision_tests.rs` 的 `Remote`：Linux x86_64，glibc 2.31）。
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "测试直接准备文件系统夹具与进程"
)]

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_cli_exec_transport::{ProvisionError, ProvisionStep, Provisioner};

use crate::artifact::Platform;
use crate::provision_tests::{Remote, mode, wanted};
use crate::{ArtifactSource, DownloadError, SshProvisioner};

/// 假的注册表：每个版本下载的结果、本机缓存里已有的版本；记下每次下载要的版本。
#[derive(Default)]
struct Registry {
    cached: HashMap<String, PathBuf>,
    published: HashMap<String, Result<PathBuf, DownloadError>>,
    downloads: Mutex<Vec<String>>,
}

impl Registry {
    fn downloads(&self) -> Vec<String> {
        self.downloads.lock().unwrap().clone()
    }
}

#[async_trait]
impl ArtifactSource for Registry {
    async fn cached(&self, version: &str, _platform: Platform) -> Option<PathBuf> {
        self.cached.get(version).cloned()
    }

    async fn download(&self, version: &str, platform: Platform) -> Result<PathBuf, DownloadError> {
        assert_eq!(platform, Platform::LinuxX86_64);
        self.downloads.lock().unwrap().push(version.to_owned());
        self.published
            .get(version)
            .cloned()
            .unwrap_or_else(|| Err(DownloadError::Failed(format!("{version}: no route"))))
    }
}

/// 本机版本 0.2.0 不在注册表里，latest 是 `latest`。
fn unpublished(latest: Option<&str>) -> Result<PathBuf, DownloadError> {
    Err(DownloadError::Unpublished {
        reason: "npm package @mbearo/zcode-linux-x64-gnu version 0.2.0 is missing".to_owned(),
        latest: latest.map(str::to_owned),
    })
}

/// 下载来的“执行器”：试跑的 `executor status --json` 写一行状态（版本 `version`、协议 `protocol`），退出码 3（常驻
/// 进程没在跑）。探测时也照样回答。
fn executor(remote: &Remote, name: &str, version: &str, protocol: u32) -> PathBuf {
    let path = remote.root.path().join(name);
    let line = format!(
        r#"{{"version":"{version}","protocol":{protocol},"executable":"/x/\"protocol\":9","resident":null}}"#
    );
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\n[ \"$1 $2 $3\" = 'executor status --json' ] && {{ printf '%s\\n' '{line}'; exit 3; }}\nexit 9\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn bin(remote: &Remote, version: &str) -> PathBuf {
    remote
        .home()
        .join(format!(".zcode/rust-cli/exec/bin/{version}/zcode-rs"))
}

/// 远端 `bin/` 下所有的文件（相对路径）。
fn remote_files(remote: &Remote) -> Vec<String> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, base, out);
            } else {
                out.push(path.strip_prefix(base).unwrap().display().to_string());
            }
        }
    }
    let base = remote.home().join(".zcode/rust-cli/exec/bin");
    let mut out = Vec::new();
    walk(&base, &base, &mut out);
    out.sort();
    out
}

/// 远端已有的一个候选：`executor status --json` 回答 `report`。
fn candidate(remote: &Remote, version: &str, report: &str) -> PathBuf {
    let path = bin(remote, version);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, format!("#!/bin/sh\nprintf '%s\\n' '{report}'\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}

async fn provision(
    remote: &Remote,
    registry: Arc<Registry>,
) -> (Result<String, ProvisionError>, Vec<ProvisionStep>) {
    let provisioner =
        SshProvisioner::new(remote.target("dev-a"), wanted(), Vec::new()).with_source(registry);
    let seen = Mutex::new(Vec::new());
    let record = |step: ProvisionStep| seen.lock().unwrap().push(step);
    let result = provisioner
        .provision_reporting(&record)
        .await
        .map(|provisioned| provisioned.program);
    (result, seen.into_inner().unwrap())
}

#[tokio::test]
async fn only_the_latest_is_published_and_it_speaks_our_protocol() {
    let remote = Remote::new();
    let latest = executor(&remote, "latest", "0.1.9", 1);
    let registry = Arc::new(Registry {
        published: HashMap::from([
            ("0.2.0".to_owned(), unpublished(Some("0.1.9"))),
            ("0.1.9".to_owned(), Ok(latest.clone())),
        ]),
        ..Registry::default()
    });
    let (program, steps) = provision(&remote, registry.clone()).await;
    // 按实际版本放：bin/0.1.9/，不是本机的 0.2.0。
    assert_eq!(program.unwrap(), bin(&remote, "0.1.9").to_string_lossy());
    assert_eq!(registry.downloads(), ["0.2.0", "0.1.9"]);
    let bytes = std::fs::read(&latest).unwrap();
    // “下载执行器”只报一次。
    assert_eq!(
        steps,
        [
            ProvisionStep::Downloading,
            ProvisionStep::Uploading {
                bytes: bytes.len() as u64
            }
        ]
    );
    assert_eq!(std::fs::read(bin(&remote, "0.1.9")).unwrap(), bytes);
    assert_eq!(mode(&bin(&remote, "0.1.9")), 0o700);
    // 试跑的临时文件与输出都不留。
    assert_eq!(remote_files(&remote), ["0.1.9/zcode-rs"]);
}

#[tokio::test]
async fn a_latest_with_another_protocol_is_refused_and_leaves_nothing() {
    let remote = Remote::new();
    let latest = executor(&remote, "latest", "0.1.9", 2);
    let registry = Arc::new(Registry {
        published: HashMap::from([
            ("0.2.0".to_owned(), unpublished(Some("0.1.9"))),
            ("0.1.9".to_owned(), Ok(latest)),
        ]),
        ..Registry::default()
    });
    let (program, _) = provision(&remote, registry).await;
    let error = program.unwrap_err();
    assert_eq!(
        error,
        ProvisionError::LatestIncompatible {
            version: "0.2.0".to_owned(),
            protocol: 1,
            latest: "0.1.9".to_owned(),
            latest_protocol: 2,
            target: "x86_64-unknown-linux-gnu".to_owned(),
        }
    );
    let message = error.to_string();
    assert!(
        message.contains("0.2.0 (protocol 1)") && message.contains("0.1.9, speaks protocol 2"),
        "{message}"
    );
    // 改名之前就核对了：远端没有留下任何文件。
    assert!(
        remote_files(&remote).is_empty(),
        "{:?}",
        remote_files(&remote)
    );
}

#[tokio::test]
async fn a_compatible_executor_on_the_remote_wins_over_the_latest() {
    let remote = Remote::new();
    let older = candidate(
        &remote,
        "0.1.5",
        r#"{"version":"0.1.5","protocol":1,"resident":null}"#,
    );
    let registry = Arc::new(Registry {
        published: HashMap::from([("0.2.0".to_owned(), unpublished(Some("0.1.9")))]),
        ..Registry::default()
    });
    let (program, steps) = provision(&remote, registry.clone()).await;
    assert_eq!(program.unwrap(), older.to_string_lossy());
    assert_eq!(
        registry.downloads(),
        ["0.2.0"],
        "the latest is not downloaded"
    );
    assert_eq!(steps, [ProvisionStep::Downloading]);

    // 空闲而协议相同的常驻进程（版本不同）：照旧用它，不下载 latest。
    let remote = Remote::new();
    let resident = bin(&remote, "0.1.5");
    let own = candidate(
        &remote,
        "0.1.5",
        &format!(
            r#"{{"version":"0.1.5","protocol":1,"resident":{{"version":"0.1.5","protocol":1,"idle":true,"executable":"{}"}}}}"#,
            resident.display()
        ),
    );
    let registry = Arc::new(Registry {
        published: HashMap::from([("0.2.0".to_owned(), unpublished(Some("0.1.9")))]),
        ..Registry::default()
    });
    let (program, _) = provision(&remote, registry.clone()).await;
    assert_eq!(program.unwrap(), own.to_string_lossy());
    assert_eq!(registry.downloads(), ["0.2.0"]);
}

#[tokio::test]
async fn an_idle_executor_with_another_protocol_is_replaced_by_the_latest() {
    let remote = Remote::new();
    let resident = bin(&remote, "0.1.5");
    candidate(
        &remote,
        "0.1.5",
        &format!(
            r#"{{"version":"0.1.5","protocol":0,"resident":{{"version":"0.1.5","protocol":0,"idle":true,"executable":"{}"}}}}"#,
            resident.display()
        ),
    );
    let latest = executor(&remote, "latest", "0.1.9", 1);
    let registry = Arc::new(Registry {
        published: HashMap::from([
            ("0.2.0".to_owned(), unpublished(Some("0.1.9"))),
            ("0.1.9".to_owned(), Ok(latest)),
        ]),
        ..Registry::default()
    });
    let (program, _) = provision(&remote, registry.clone()).await;
    // latest 的代理会按 §19.5 把空闲的旧进程换掉。
    assert_eq!(program.unwrap(), bin(&remote, "0.1.9").to_string_lossy());
    assert_eq!(registry.downloads(), ["0.2.0", "0.1.9"]);
}

#[tokio::test]
async fn only_an_unpublished_version_falls_back_to_the_latest() {
    let cases = [
        // 网络错误、校验不对：不退。
        (
            Err(DownloadError::Failed("GET https://registry.example.com/x: connection refused".to_owned())),
            "GET https://registry.example.com/x: connection refused",
        ),
        (
            Err(DownloadError::Failed(
                "@mbearo/zcode-linux-x64-gnu 0.2.0: the download does not match the registry's sha512 integrity"
                    .to_owned(),
            )),
            "@mbearo/zcode-linux-x64-gnu 0.2.0: the download does not match the registry's sha512 integrity",
        ),
        // 不在注册表里，但没有 latest（包从来没发过），或 latest 就是本机的版本。
        (
            unpublished(None),
            "npm package @mbearo/zcode-linux-x64-gnu version 0.2.0 is missing",
        ),
        (
            unpublished(Some("0.2.0")),
            "npm package @mbearo/zcode-linux-x64-gnu version 0.2.0 is missing",
        ),
    ];
    for (result, reason) in cases {
        let remote = Remote::new();
        let latest = executor(&remote, "latest", "0.1.9", 1);
        let registry = Arc::new(Registry {
            published: HashMap::from([
                ("0.2.0".to_owned(), result),
                ("0.1.9".to_owned(), Ok(latest)),
            ]),
            ..Registry::default()
        });
        let (program, _) = provision(&remote, registry.clone()).await;
        assert_eq!(
            program,
            Err(ProvisionError::DownloadFailed {
                version: "0.2.0".to_owned(),
                target: "x86_64-unknown-linux-gnu".to_owned(),
                reason: reason.to_owned(),
            })
        );
        assert_eq!(registry.downloads(), ["0.2.0"]);
        assert!(remote_files(&remote).is_empty());
    }
}

#[tokio::test]
async fn a_cached_latest_is_uploaded_without_downloading_it_and_still_checked() {
    let remote = Remote::new();
    let cached = executor(&remote, "cached", "0.1.9", 1);
    let registry = Arc::new(Registry {
        cached: HashMap::from([("0.1.9".to_owned(), cached)]),
        published: HashMap::from([("0.2.0".to_owned(), unpublished(Some("0.1.9")))]),
        ..Registry::default()
    });
    let (program, steps) = provision(&remote, registry.clone()).await;
    assert_eq!(program.unwrap(), bin(&remote, "0.1.9").to_string_lossy());
    assert_eq!(registry.downloads(), ["0.2.0"]);
    assert!(
        matches!(
            steps.as_slice(),
            [ProvisionStep::Downloading, ProvisionStep::Uploading { .. }]
        ),
        "{steps:?}"
    );

    // 缓存里的 latest 说别的协议：照样试跑、报错，不留文件。
    let remote = Remote::new();
    let cached = executor(&remote, "cached", "0.1.9", 3);
    let registry = Arc::new(Registry {
        cached: HashMap::from([("0.1.9".to_owned(), cached)]),
        published: HashMap::from([("0.2.0".to_owned(), unpublished(Some("0.1.9")))]),
        ..Registry::default()
    });
    let (program, _) = provision(&remote, registry).await;
    assert!(
        matches!(
            program,
            Err(ProvisionError::LatestIncompatible {
                latest_protocol: 3,
                ..
            })
        ),
        "{program:?}"
    );
    assert!(remote_files(&remote).is_empty());
}

#[tokio::test]
async fn a_latest_that_cannot_be_downloaded_names_both_versions() {
    let remote = Remote::new();
    let registry = Arc::new(Registry {
        published: HashMap::from([
            ("0.2.0".to_owned(), unpublished(Some("0.0.0-placeholder"))),
            (
                "0.0.0-placeholder".to_owned(),
                Err(DownloadError::Failed(
                    "@mbearo/zcode-linux-x64-gnu 0.0.0-placeholder: the package has no bin/zcode-rs"
                        .to_owned(),
                )),
            ),
        ]),
        ..Registry::default()
    });
    let (program, _) = provision(&remote, registry.clone()).await;
    assert_eq!(
        program,
        Err(ProvisionError::DownloadFailed {
            version: "0.2.0".to_owned(),
            target: "x86_64-unknown-linux-gnu".to_owned(),
            reason: "zcode 0.2.0 is not on npm, and the latest, 0.0.0-placeholder, could not be downloaded: @mbearo/zcode-linux-x64-gnu 0.0.0-placeholder: the package has no bin/zcode-rs".to_owned(),
        })
    );
    assert_eq!(registry.downloads(), ["0.2.0", "0.0.0-placeholder"]);
}
