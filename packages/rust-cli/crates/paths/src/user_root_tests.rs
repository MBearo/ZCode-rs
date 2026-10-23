//! 用户根收拢（F55）前后的等价性：每个存储位置与收拢前的写法逐字节相同（比较 `OsStr`，不是 `Path` 的组件）。
//!
//! 右边是收拢前 `lib.rs` 的原式（`home.join(".zcode/v2")` 这样整段接上的，与 `.join(".zcode").join(…)` 分开接的，
//! 在 Windows 上分隔符不同，这里按原样写）。macOS / Linux 上两种写法的结果相同；Windows 上的形状见文件末尾的
//! `cfg(windows)` 用例（本线不改 Windows 的混用分隔符，config.md §2.3）。

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use super::*;

fn os(path: &Path) -> &OsStr {
    path.as_os_str()
}

/// 收拢前 `expand`：`base.join(expand_home(value, home))`。
fn old_expand(value: &str, home: &Path, base: &Path) -> PathBuf {
    base.join(expand_home(value, home))
}

fn homes() -> Vec<PathBuf> {
    // 末尾带分隔符与相对的 home（HOME 可以是任意值）也要一致。
    ["/Users/dev", "/Users/dev/", "rel/home"]
        .into_iter()
        .map(PathBuf::from)
        .collect()
}

#[test]
fn default_storage_locations_are_byte_identical_to_the_old_formulas() {
    let base = Path::new("/work/repo");
    for home in homes() {
        let paths = StoragePaths::resolve(&StorageSettings::default(), &home, base);
        let root = old_expand("~/.zcode", &home, base);
        let config = home.join(".zcode/v2");
        assert_eq!(
            os(paths.database().as_path()),
            os(&old_expand("~/.zcode/cli/db/db.sqlite", &home, base))
        );
        assert_eq!(
            os(paths.artifacts().as_path()),
            os(&root.join("cli/artifacts"))
        );
        assert_eq!(
            os(paths.memory_dir("/work/repo").as_path()),
            os(&root.join("projects").join("-work-repo").join("memory"))
        );
        assert_eq!(
            os(paths.task_list("s1").as_path()),
            os(&root.join("tasks").join("s1.json"))
        );
        assert_eq!(os(paths.logs().as_path()), os(&root.join("rust-cli/log")));
        assert_eq!(
            os(paths.peers().as_path()),
            os(&root.join("rust-cli/peers"))
        );
        assert_eq!(
            os(paths.update_cache().as_path()),
            os(&root.join("rust-cli/version.json"))
        );
        assert_eq!(os(paths.zcode_config().as_path()), os(&config));
        assert_eq!(
            os(paths.provider_config().as_path()),
            os(&config.join("provider_config.json"))
        );
        assert_eq!(
            os(paths.credentials().as_path()),
            os(&config.join("credentials.json"))
        );
        assert_eq!(
            os(paths.telemetry_state().as_path()),
            os(&config.join("telemetry-state.json"))
        );
        assert_eq!(
            os(paths.telemetry_lock().as_path()),
            os(&config.join("telemetry-state.lock"))
        );
        assert_eq!(
            os(paths.builtin_provider_cache().as_path()),
            os(&config.join("runtime").join("provider"))
        );
        assert_eq!(
            os(paths.tui_history().as_path()),
            os(&config.join("tui").join("history.jsonl"))
        );
        assert_eq!(
            os(paths.settings().as_path()),
            os(&home.join(".zcode/v2").join("setting.json"))
        );
        assert_eq!(
            os(paths.cli_config().as_path()),
            os(&home.join(".zcode/cli/config.json"))
        );
        for blank in [None, Some(""), Some("  ")] {
            assert_eq!(
                os(paths.hook_trust_store(blank).as_path()),
                os(&home
                    .join(".zcode")
                    .join("security/workspace-hook-trust-v1.json"))
            );
        }
        assert_eq!(
            os(paths.plugin_storage(None, base).as_path()),
            os(&old_expand("~/.zcode", &home, base)
                .join("cli")
                .join("plugins"))
        );
    }
}

#[test]
fn configured_locations_are_byte_identical_to_the_old_formulas() {
    let base = Path::new("/work/repo");
    let home = Path::new("/Users/dev");
    let settings = StorageSettings {
        session_db_path: Some("~/db/custom.sqlite"),
        storage_dir: Some("data"),
    };
    let paths = StoragePaths::resolve(&settings, home, base);
    let root = old_expand("data", home, base);
    assert_eq!(
        os(paths.database().as_path()),
        os(&old_expand("~/db/custom.sqlite", home, base))
    );
    assert_eq!(
        os(paths.artifacts().as_path()),
        os(&root.join("cli/artifacts"))
    );
    assert_eq!(os(paths.logs().as_path()), os(&root.join("rust-cli/log")));
    // `ZCODE_DATA_BASE_DIR` 只换账号与 Provider 的配置根，设置文件仍在 home 下。
    let data = Path::new("/srv/data");
    let moved = paths.clone().with_data_base_dir(data);
    assert_eq!(
        os(moved.zcode_config().as_path()),
        os(&data.join(".zcode/v2"))
    );
    assert_eq!(
        os(moved.credentials().as_path()),
        os(&data.join(".zcode/v2").join("credentials.json"))
    );
    assert_eq!(
        os(moved.settings().as_path()),
        os(&home.join(".zcode/v2").join("setting.json"))
    );
    // 信任记录与插件存储跟随调用方给的 `storage.dir`。
    assert_eq!(
        os(paths.hook_trust_store(Some("~/state")).as_path()),
        os(&home
            .join(expand_home("~/state", home))
            .join("security/workspace-hook-trust-v1.json"))
    );
    assert_eq!(
        os(paths.plugin_storage(Some("~/x/cli"), base).as_path()),
        os(&old_expand("~/x/cli", home, base).join("plugins"))
    );
    assert_eq!(
        os(paths.plugin_storage(Some("other"), base).as_path()),
        os(&old_expand("other", home, base).join("cli").join("plugins"))
    );
}

#[test]
fn the_user_root_is_computed_in_one_place() {
    for home in homes() {
        assert_eq!(os(&user_path(&home, "")), os(&home.join(".zcode")));
        assert_eq!(os(&user_path(&home, "v2")), os(&home.join(".zcode/v2")));
        assert_eq!(
            os(&user_path(&home, "cli/db/db.sqlite")),
            os(&home.join(".zcode/cli/db/db.sqlite"))
        );
    }
    assert_eq!(DOT_DIR, ".zcode");
}

#[cfg(unix)]
#[test]
fn unix_defaults_spell_the_documented_strings() {
    let paths = StoragePaths::resolve(
        &StorageSettings::default(),
        Path::new("/Users/dev"),
        Path::new("/work/repo"),
    );
    let text = |path: &StoragePath| path.as_path().to_string_lossy().into_owned();
    assert_eq!(text(paths.database()), "/Users/dev/.zcode/cli/db/db.sqlite");
    assert_eq!(text(paths.artifacts()), "/Users/dev/.zcode/cli/artifacts");
    assert_eq!(
        text(&paths.cli_config()),
        "/Users/dev/.zcode/cli/config.json"
    );
    assert_eq!(text(paths.settings()), "/Users/dev/.zcode/v2/setting.json");
    assert_eq!(
        text(&paths.provider_config()),
        "/Users/dev/.zcode/v2/provider_config.json"
    );
    assert_eq!(text(&paths.logs()), "/Users/dev/.zcode/rust-cli/log");
    assert_eq!(
        user_path(Path::new("/Users/dev"), "").to_string_lossy(),
        "/Users/dev/.zcode"
    );
}

/// Windows 上现有的形状（混用分隔符，F55 不改；只在 Windows 上编译运行）。
#[cfg(windows)]
#[test]
fn windows_defaults_keep_their_current_shape() {
    let paths = StoragePaths::resolve(
        &StorageSettings::default(),
        Path::new(r"C:\Users\dev"),
        Path::new(r"C:\work\repo"),
    );
    let text = |path: &StoragePath| path.as_path().to_string_lossy().into_owned();
    assert_eq!(
        text(paths.database()),
        r"C:\Users\dev\.zcode/cli/db/db.sqlite"
    );
    assert_eq!(
        text(paths.artifacts()),
        r"C:\Users\dev\.zcode\cli/artifacts"
    );
    assert_eq!(
        text(&paths.cli_config()),
        r"C:\Users\dev\.zcode/cli/config.json"
    );
    assert_eq!(
        text(paths.settings()),
        r"C:\Users\dev\.zcode/v2\setting.json"
    );
    assert_eq!(
        text(&paths.provider_config()),
        r"C:\Users\dev\.zcode/v2\provider_config.json"
    );
    assert_eq!(text(&paths.logs()), r"C:\Users\dev\.zcode\rust-cli/log");
    assert_eq!(
        user_path(Path::new(r"C:\Users\dev"), "v2").to_string_lossy(),
        r"C:\Users\dev\.zcode/v2"
    );
}
