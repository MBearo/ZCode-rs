use std::path::{Path, PathBuf};

use super::*;

fn resolve(session_db_path: Option<&str>, storage_dir: Option<&str>) -> StoragePaths {
    StoragePaths::resolve(
        &StorageSettings {
            session_db_path,
            storage_dir,
        },
        Path::new("/home/dev"),
        Path::new("/work/repo"),
    )
}

#[test]
fn defaults_follow_the_node_cli() {
    let paths = resolve(None, None);
    assert_eq!(
        paths.database().as_path(),
        Path::new("/home/dev/.zcode/cli/db/db.sqlite")
    );
    assert_eq!(
        paths.artifacts().as_path(),
        Path::new("/home/dev/.zcode/cli/artifacts")
    );
}

#[test]
fn settings_expand_home_and_resolve_relative_paths_against_the_base() {
    let paths = resolve(Some("~/db/custom.sqlite"), Some("data"));
    assert_eq!(
        paths.database().as_path(),
        Path::new("/home/dev/db/custom.sqlite")
    );
    assert_eq!(
        paths.artifacts().as_path(),
        Path::new("/work/repo/data/cli/artifacts")
    );
}

#[test]
fn absolute_settings_are_kept() {
    let paths = resolve(Some("/var/db.sqlite"), Some("/srv/zcode"));
    assert_eq!(paths.database().as_path(), Path::new("/var/db.sqlite"));
    assert_eq!(
        paths.artifacts().as_path(),
        PathBuf::from("/srv/zcode/cli/artifacts")
    );
}

#[test]
fn joined_storage_paths_stay_storage_paths() {
    let paths = resolve(None, None);
    let session = paths.artifacts().join("sess_1");
    assert_eq!(
        session.as_path(),
        Path::new("/home/dev/.zcode/cli/artifacts/sess_1")
    );
}

#[test]
fn memory_lives_under_the_storage_root() {
    let paths = resolve(None, None);
    assert_eq!(
        paths.memory_dir("/work/repo").as_path(),
        Path::new("/home/dev/.zcode/projects/-work-repo/memory")
    );
    assert_eq!(sanitize_path("C:\\a b/c.d"), "C--a-b-c-d");
    // 代理对是两个 UTF-16 码元，各换成一个 `-`。
    assert_eq!(sanitize_path("/x/😀"), "-x---");
}

#[test]
fn long_paths_are_truncated_with_the_node_hash() {
    let long = format!("/{}", "a".repeat(250));
    let name = sanitize_path(&long);
    assert!(name.starts_with(&format!("-{}", "a".repeat(199))));
    assert_eq!(name.len(), 200 + 1 + name.rsplit('-').next().unwrap().len());
    // Node：Math.abs(djb2Hash("/aaa…")).toString(36)，用 node -e 算出的值。
    assert_eq!(name.rsplit('-').next().unwrap(), "feo44x");
}

#[test]
fn account_and_provider_files_follow_node() {
    let paths = resolve(None, None);
    assert_eq!(
        paths.provider_config().as_path(),
        Path::new("/home/dev/.zcode/v2/provider_config.json")
    );
    assert_eq!(
        paths.credentials().as_path(),
        Path::new("/home/dev/.zcode/v2/credentials.json")
    );
    assert_eq!(
        paths.builtin_provider_cache().as_path(),
        Path::new("/home/dev/.zcode/v2/runtime/provider")
    );
    assert_eq!(
        paths.settings().as_path(),
        Path::new("/home/dev/.zcode/v2/setting.json")
    );
    // ZCODE_DATA_BASE_DIR 只移动账号与 Provider 配置，设置文件仍在 home 下。
    let moved = paths.with_data_base_dir(Path::new("/data"));
    assert_eq!(
        moved.credentials().as_path(),
        Path::new("/data/.zcode/v2/credentials.json")
    );
    assert_eq!(
        moved.settings().as_path(),
        Path::new("/home/dev/.zcode/v2/setting.json")
    );
}

#[test]
fn the_tui_history_follows_the_config_root() {
    let paths = resolve(None, None);
    assert_eq!(
        paths.tui_history().as_path(),
        Path::new("/home/dev/.zcode/v2/tui/history.jsonl")
    );
    let moved = paths.with_data_base_dir(Path::new("/data"));
    assert_eq!(
        moved.tui_history().as_path(),
        Path::new("/data/.zcode/v2/tui/history.jsonl")
    );
}

#[test]
fn task_lists_live_under_the_storage_root() {
    assert_eq!(
        resolve(None, None).task_list("sess_1").as_path(),
        Path::new("/home/dev/.zcode/tasks/sess_1.json")
    );
    assert_eq!(
        resolve(None, Some("data")).task_list("sess_1").as_path(),
        Path::new("/work/repo/data/tasks/sess_1.json")
    );
}

#[test]
fn run_logs_live_under_the_storage_root() {
    assert_eq!(
        resolve(None, None).logs().as_path(),
        Path::new("/home/dev/.zcode/rust-cli/log")
    );
    assert_eq!(
        resolve(None, Some("data")).logs().as_path(),
        Path::new("/work/repo/data/rust-cli/log")
    );
}

#[test]
fn the_peer_registry_lives_under_the_storage_root() {
    assert_eq!(
        resolve(None, None).peers().as_path(),
        Path::new("/home/dev/.zcode/rust-cli/peers")
    );
}

/// 自动更新的缓存（docs/specs/rust-cli/update.md §4，F48）：本工程专用的目录，随 `storage.dir`。
#[test]
fn the_update_cache_lives_under_the_storage_root() {
    assert_eq!(
        resolve(None, None).update_cache().as_path(),
        Path::new("/home/dev/.zcode/rust-cli/version.json")
    );
    assert_eq!(
        resolve(None, Some("data")).update_cache().as_path(),
        Path::new("/work/repo/data/rust-cli/version.json")
    );
}

#[test]
fn the_cli_config_and_the_hook_trust_store_follow_the_node_cli() {
    let paths = resolve(None, Some("data"));
    assert_eq!(
        paths.cli_config().as_path(),
        Path::new("/home/dev/.zcode/cli/config.json")
    );
    assert_eq!(
        paths.hook_trust_store(None).as_path(),
        Path::new("/home/dev/.zcode/security/workspace-hook-trust-v1.json")
    );
    // 相对路径相对 home（不随 cwd 漂移），`~/` 展开，绝对路径原样，空白视为没有。
    assert_eq!(
        paths.hook_trust_store(Some("store")).as_path(),
        Path::new("/home/dev/store/security/workspace-hook-trust-v1.json")
    );
    assert_eq!(
        paths.hook_trust_store(Some("~/x")).as_path(),
        Path::new("/home/dev/x/security/workspace-hook-trust-v1.json")
    );
    assert_eq!(
        paths.hook_trust_store(Some("/srv/z")).as_path(),
        Path::new("/srv/z/security/workspace-hook-trust-v1.json")
    );
    assert_eq!(
        paths.hook_trust_store(Some("  ")).as_path(),
        Path::new("/home/dev/.zcode/security/workspace-hook-trust-v1.json")
    );
}

#[test]
fn the_plugin_storage_follows_the_node_cli() {
    let paths = resolve(None, None);
    let base = Path::new("/work/repo");
    // Node `getPluginStorageRoot(getCliStorageRoot(resolvePath(storage.dir)))`。
    assert_eq!(
        paths.plugin_storage(None, base).as_path(),
        Path::new("/home/dev/.zcode/cli/plugins")
    );
    assert_eq!(
        paths.plugin_storage(Some("~/z"), base).as_path(),
        Path::new("/home/dev/z/cli/plugins")
    );
    // 相对路径相对基准目录（Node `resolve` 相对进程 cwd），末段已是 `cli` 时不再加一层。
    assert_eq!(
        paths.plugin_storage(Some("data"), base).as_path(),
        Path::new("/work/repo/data/cli/plugins")
    );
    assert_eq!(
        paths.plugin_storage(Some("/srv/z/cli"), base).as_path(),
        Path::new("/srv/z/cli/plugins")
    );
    assert_eq!(
        paths.plugin_storage(Some(" "), base).as_path(),
        Path::new("/home/dev/.zcode/cli/plugins")
    );
}

/// 设备身份的文件与锁（工作线 F52，platform.md §10）：与 Node CLI `resolveCliTelemetryStateFile` 相同，跟随
/// `ZCODE_DATA_BASE_DIR`。
#[test]
fn the_telemetry_state_follows_the_data_base_dir() {
    let paths = resolve(None, None);
    assert_eq!(
        paths.telemetry_state().as_path(),
        Path::new("/home/dev/.zcode/v2/telemetry-state.json")
    );
    assert_eq!(
        paths.telemetry_lock().as_path(),
        Path::new("/home/dev/.zcode/v2/telemetry-state.lock")
    );
    let moved = paths.with_data_base_dir(Path::new("/data"));
    assert_eq!(
        moved.telemetry_state().as_path(),
        Path::new("/data/.zcode/v2/telemetry-state.json")
    );
    assert_eq!(
        moved.telemetry_lock().as_path(),
        Path::new("/data/.zcode/v2/telemetry-state.lock")
    );
}
