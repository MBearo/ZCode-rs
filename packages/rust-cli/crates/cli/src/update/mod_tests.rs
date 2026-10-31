#![allow(clippy::disallowed_methods, reason = "测试：临时目录、读回缓存文件")]
use std::path::Path;

use rust_cli_kernel_api::testing::FixedClock;

use super::fake_registry_tests::{FakeRegistry, egress, package};
use super::*;

const HOUR_MS: u64 = 60 * 60 * 1000;
/// 2026-10-20T08:00:00.000Z。
const NOW_MS: u64 = 1_792_483_200_000;
const ENTRY: &str = "/@scope%2fzcode";
const PLATFORM: &str = "/@scope%2fzcode-darwin-arm64";

fn marked() -> Vec<(String, String)> {
    [
        ("ZCODE_MANAGED_BY_NPM", "1"),
        ("ZCODE_MANAGED_PACKAGE", "@scope/zcode"),
        (
            "ZCODE_MANAGED_PLATFORM_PACKAGE",
            "@scope/zcode-darwin-arm64",
        ),
    ]
    .iter()
    .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
    .collect()
}

/// registry 上的最新版本是 `latest`（入口包与平台包都发了）。
fn registry(latest: &str) -> FakeRegistry {
    FakeRegistry::start(vec![
        (
            ENTRY.to_owned(),
            200,
            package("@scope/zcode", latest, &[latest]),
        ),
        (
            PLATFORM.to_owned(),
            200,
            package("@scope/zcode-darwin-arm64", latest, &[latest]),
        ),
    ])
}

fn write_cache(path: &Path, latest: &str, checked_ms: u64, dismissed: Option<&str>) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let info = cache::VersionInfo {
        latest_version: latest.to_owned(),
        last_checked_at_ms: checked_ms as i64,
        dismissed_version: dismissed.map(str::to_owned),
    };
    std::fs::write(path, info.render()).unwrap();
}

async fn start(env: &[(String, String)], enabled: bool, cache: &Path, url: &str) -> Updates {
    start_at(env, enabled, cache, url, "0.1.46").await
}

async fn start_at(
    env: &[(String, String)],
    enabled: bool,
    cache: &Path,
    url: &str,
    version: &str,
) -> Updates {
    Updates::start(Inputs {
        env,
        enabled,
        version,
        cache: cache.to_path_buf(),
        egress: Arc::new(egress().await),
        clock: Arc::new(FixedClock(NOW_MS)),
        registry: url.to_owned(),
    })
    .await
}

fn expected(latest: &str) -> UpdateInfo {
    UpdateInfo {
        current: "0.1.46".to_owned(),
        latest: latest.to_owned(),
        command: "npm install -g @scope/zcode".to_owned(),
        notes_url: "https://www.npmjs.com/package/@scope/zcode".to_owned(),
    }
}

/// update.md §2、§3：没有标记时不读缓存、不联网、不弹、不提示（本地构建与以前完全相同）。
#[tokio::test]
async fn without_a_marker_nothing_is_read_or_fetched() {
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("rust-cli").join("version.json");
    let server = registry("0.1.47");
    // 缓存里有更新的版本也不弹：没有标记就不读它。
    write_cache(&cache, "0.1.47", NOW_MS - 30 * HOUR_MS, None);
    let mut updates = start(&[], true, &cache, &server.url).await;
    assert_eq!(updates.prompt(), None);
    assert_eq!(updates.notice(), None);
    assert!(updates.install().is_none());
    assert!(!updates.refreshed().await);
    // 只有包名、没有标记：同样不检查。
    let half = vec![(
        "ZCODE_MANAGED_PACKAGE".to_owned(),
        "@scope/zcode".to_owned(),
    )];
    let mut updates = start(&half, true, &cache, &server.url).await;
    assert!(!updates.refreshed().await);
    assert!(server.seen().is_empty(), "{:?}", server.seen());
    // 缓存文件原样（没有被刷新写过）。
    assert!(
        std::fs::read_to_string(&cache)
            .unwrap()
            .contains("\"last_checked_at\":\"2026-10-19T02:00:00.000Z\"")
    );
}

#[tokio::test]
async fn the_switch_and_source_builds_turn_the_check_off() {
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("version.json");
    let server = registry("0.1.47");
    for (enabled, version) in [(false, "0.1.46"), (true, "0.0.0")] {
        let mut updates = start_at(&marked(), enabled, &cache, &server.url, version).await;
        assert_eq!(updates.prompt(), None);
        assert_eq!(updates.notice(), None);
        assert!(!updates.refreshed().await);
    }
    assert!(server.seen().is_empty());
    assert!(!cache.exists());
}

/// 没有缓存：本次不弹（只用已有的缓存），后台刷新写下最新版本，下次启动才弹。
#[tokio::test]
async fn a_missing_cache_is_refreshed_in_the_background_for_the_next_start() {
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("rust-cli").join("version.json");
    let server = registry("0.1.47");
    let mut updates = start(&marked(), true, &cache, &server.url).await;
    assert_eq!(updates.prompt(), None);
    assert_eq!(updates.notice(), None);
    assert!(updates.install().is_some());
    assert!(updates.refreshed().await);
    let paths: Vec<String> = server.seen().into_iter().map(|seen| seen.path).collect();
    assert_eq!(paths, [ENTRY, PLATFORM]);
    assert_eq!(
        std::fs::read_to_string(&cache).unwrap(),
        "{\"latest_version\":\"0.1.47\",\"last_checked_at\":\"2026-10-20T08:00:00.000Z\",\"dismissed_version\":null}\n"
    );
    let next = start(&marked(), true, &cache, &server.url).await;
    assert_eq!(next.prompt(), Some(expected("0.1.47")));
    assert_eq!(next.notice(), Some(expected("0.1.47")));
}

#[tokio::test]
async fn a_fresh_cache_is_used_without_fetching() {
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("version.json");
    let server = registry("0.1.48");
    write_cache(&cache, "0.1.47", NOW_MS - 19 * HOUR_MS, None);
    let mut updates = start(&marked(), true, &cache, &server.url).await;
    assert_eq!(updates.prompt(), Some(expected("0.1.47")));
    assert_eq!(updates.notice(), Some(expected("0.1.47")));
    assert!(!updates.refreshed().await);
    assert!(server.seen().is_empty());
    // 缓存的版本不比当前新：不弹、不提示。
    for latest in ["0.1.46", "0.1.45", "0.2.0-beta.1"] {
        write_cache(&cache, latest, NOW_MS - HOUR_MS, None);
        let updates = start(&marked(), true, &cache, &server.url).await;
        assert_eq!(updates.prompt(), None, "{latest}");
        assert_eq!(updates.notice(), None, "{latest}");
    }
}

/// 过期的缓存：本次照它弹，同时后台刷新；刷新保留不再提醒的版本。
#[tokio::test]
async fn a_stale_cache_still_decides_this_start_and_is_refreshed() {
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("version.json");
    let server = registry("0.1.48");
    write_cache(&cache, "0.1.47", NOW_MS - 21 * HOUR_MS, Some("0.1.45"));
    let mut updates = start(&marked(), true, &cache, &server.url).await;
    assert_eq!(updates.prompt(), Some(expected("0.1.47")));
    assert!(updates.refreshed().await);
    let written = cache::read(&cache).await.unwrap();
    assert_eq!(written.latest_version, "0.1.48");
    assert_eq!(written.last_checked_at_ms, NOW_MS as i64);
    assert_eq!(written.dismissed_version.as_deref(), Some("0.1.45"));
}

/// 核对不过（平台包还没有这个版本）：不写缓存，下次启动再试。
#[tokio::test]
async fn a_failed_check_keeps_the_old_cache() {
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("version.json");
    let server = FakeRegistry::start(vec![
        (
            ENTRY.to_owned(),
            200,
            package("@scope/zcode", "0.1.48", &["0.1.48"]),
        ),
        (
            PLATFORM.to_owned(),
            200,
            package("@scope/zcode-darwin-arm64", "0.1.47", &["0.1.47"]),
        ),
    ]);
    write_cache(&cache, "0.1.47", NOW_MS - 21 * HOUR_MS, None);
    let mut updates = start(&marked(), true, &cache, &server.url).await;
    assert!(updates.refreshed().await);
    let kept = cache::read(&cache).await.unwrap();
    assert_eq!(kept.latest_version, "0.1.47");
    assert_eq!(kept.last_checked_at_ms, (NOW_MS - 21 * HOUR_MS) as i64);
}

/// 选过“不再提醒”的版本不弹，会话里仍提示（会话提示不看 dismissed）；有更新的版本时又弹。
#[tokio::test]
async fn skip_until_next_version_hides_only_the_prompt_for_that_version() {
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("version.json");
    let server = registry("0.1.47");
    write_cache(&cache, "0.1.47", NOW_MS - HOUR_MS, None);
    let updates = start(&marked(), true, &cache, &server.url).await;
    assert!(updates.prompt().is_some());
    assert_eq!(
        updates.choose(UpdateChoice::SkipUntilNext).await,
        Next::Continue
    );
    let written = cache::read(&cache).await.unwrap();
    assert_eq!(written.dismissed_version.as_deref(), Some("0.1.47"));
    assert_eq!(written.last_checked_at_ms, (NOW_MS - HOUR_MS) as i64);
    let again = start(&marked(), true, &cache, &server.url).await;
    assert_eq!(again.prompt(), None);
    assert_eq!(again.notice(), Some(expected("0.1.47")));
    write_cache(&cache, "0.1.48", NOW_MS - HOUR_MS, Some("0.1.47"));
    let newer = start(&marked(), true, &cache, &server.url).await;
    assert_eq!(newer.prompt(), Some(expected("0.1.48")));
}

#[tokio::test]
async fn each_choice_maps_to_its_outcome() {
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("version.json");
    write_cache(&cache, "0.1.47", NOW_MS - HOUR_MS, None);
    let updates = start(&marked(), true, &cache, "http://127.0.0.1:9").await;
    assert_eq!(updates.choose(UpdateChoice::UpdateNow).await, Next::Update);
    assert_eq!(updates.choose(UpdateChoice::Skip).await, Next::Continue);
    // Skip 与 Update now 不写缓存。
    assert_eq!(cache::read(&cache).await.unwrap().dismissed_version, None);
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|arg| (*arg).to_owned()).collect()
}

#[test]
fn the_update_command_is_parsed_like_commander() {
    assert_eq!(parse(&args(&["update"])), Some(Ok(Command::Run)));
    assert_eq!(parse(&args(&["upgrade"])), Some(Ok(Command::Run)));
    assert_eq!(parse(&args(&["update", "--help"])), Some(Ok(Command::Help)));
    assert_eq!(
        parse(&args(&["update", "--nope", "-h"])),
        Some(Ok(Command::Help))
    );
    assert_eq!(
        parse(&args(&["update", "--nope"])),
        Some(Err("error: unknown option '--nope'".to_owned()))
    );
    assert_eq!(
        parse(&args(&["update", "now", "please"])),
        Some(Err(
            "error: too many arguments for 'update'. Expected 0 arguments but got 2.".to_owned()
        ))
    );
    assert_eq!(parse(&args(&[])), None);
    assert_eq!(parse(&args(&["-p", "update"])), None);
    assert_eq!(parse(&args(&["login"])), None);
}

#[tokio::test]
async fn the_update_command_without_a_marker_asks_for_a_manual_update() {
    let mut out = Vec::new();
    let error = command(Command::Run, &[], &mut out).await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "Error: Could not detect the ZCode installation method. Please update manually: https://www.npmjs.com/package/@mbearo/zcode"
    );
    assert!(out.is_empty());
    let mut help = Vec::new();
    assert_eq!(command(Command::Help, &[], &mut help).await.unwrap(), 0);
    assert_eq!(
        String::from_utf8(help).unwrap(),
        "Usage: zcode update|upgrade [options]\n\nUpdate ZCode to the latest version\n\nOptions:\n  -h, --help  Display help for command\n"
    );
}

/// 交接变量（update.md §8.1，F66）：`zcode update` 写 `latest`；选择屏的 Update now 写选择屏上的最新版本。都以 0 返回、
/// 不起包管理器（标记里的 npm 不在 PATH 上，真起了会报错）。
#[tokio::test]
async fn the_handoff_carries_latest_for_the_command_and_the_prompted_version_for_update_now() {
    let dir = tempfile::tempdir().unwrap();
    let handoff_path = dir.path().join("zcode-update-1.json");
    let mut env = marked();
    env.push((
        handoff::VARIABLE.to_owned(),
        handoff_path.display().to_string(),
    ));
    env.push(("PATH".to_owned(), dir.path().display().to_string()));
    let mut out = Vec::new();
    assert_eq!(command(Command::Run, &env, &mut out).await.unwrap(), 0);
    assert!(out.is_empty());
    assert_eq!(
        std::fs::read_to_string(&handoff_path).unwrap(),
        "{\"version\":\"latest\"}\n"
    );
    std::fs::remove_file(&handoff_path).unwrap();

    let cache = dir.path().join("version.json");
    write_cache(&cache, "0.1.47", NOW_MS - HOUR_MS, None);
    let updates = start(&env, true, &cache, "http://127.0.0.1:9").await;
    let stop = tokio_util::sync::CancellationToken::new();
    let ask = async |info: UpdateInfo| {
        assert_eq!(info, expected("0.1.47"));
        Ok::<_, anyhow::Error>(UpdateChoice::UpdateNow)
    };
    assert_eq!(
        offer(Some(&updates), ask, &stop, &env).await.unwrap(),
        Some(0)
    );
    assert_eq!(
        std::fs::read_to_string(&handoff_path).unwrap(),
        "{\"version\":\"0.1.47\"}\n"
    );
}
