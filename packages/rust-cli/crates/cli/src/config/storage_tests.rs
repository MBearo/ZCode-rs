//! `storage.dir` 与 `storage.sessionDbPath` 的层级（Node `createConfig` + `parseEnvConfig`）。
use rust_cli_kernel_api::permission::RuleSource;
use serde_json::{Map, Value, json};

use super::*;

fn object(value: Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap()
}

fn layers<'a>(user: &'a Map<String, Value>, projects: &'a [Map<String, Value>]) -> Vec<Layer<'a>> {
    std::iter::once(Layer {
        source: RuleSource::UserSettings,
        object: user,
        path: "user",
    })
    .chain(projects.iter().map(|object| Layer {
        source: RuleSource::ProjectSettings,
        object,
        path: "project",
    }))
    .collect()
}

fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

#[test]
fn the_storage_dir_follows_the_layers_and_the_environment() {
    let user = object(json!({"storage": {"dir": "~/u"}}));
    let projects = [object(json!({"storage": {"dir": "p"}}))];
    let dir = |layers: &[Layer<'_>], env: &[(String, String)]| {
        StorageLayers::from_layers(layers, env).dir
    };
    assert_eq!(dir(&layers(&user, &[]), &[]).as_deref(), Some("~/u"));
    assert_eq!(dir(&layers(&user, &projects), &[]).as_deref(), Some("p"));
    let set = env(&[("ZCODE_STORAGE_DIR", "/srv/zcode")]);
    assert_eq!(
        dir(&layers(&user, &projects), &set).as_deref(),
        Some("/srv/zcode")
    );
    // Node 的 merge 跳过空值：空的环境变量不覆盖配置文件。
    let blank = env(&[("ZCODE_STORAGE_DIR", "")]);
    assert_eq!(dir(&layers(&user, &projects), &blank).as_deref(), Some("p"));
    let empty = Map::new();
    assert_eq!(dir(&layers(&empty, &[]), &[]), None);
}

#[test]
fn the_session_db_path_follows_the_layers_and_the_last_env_name() {
    let user = object(json!({"storage": {"sessionDbPath": "~/u.sqlite"}}));
    let projects = [object(json!({"storage": {"sessionDbPath": "p.sqlite"}}))];
    let db = |layers: &[Layer<'_>], env: &[(String, String)]| {
        StorageLayers::from_layers(layers, env).session_db_path
    };
    assert_eq!(db(&layers(&user, &[]), &[]).as_deref(), Some("~/u.sqlite"));
    assert_eq!(
        db(&layers(&user, &projects), &[]).as_deref(),
        Some("p.sqlite")
    );
    // 两个环境变量都有时，环境里后出现的覆盖（Node 按 `Object.entries(process.env)` 的顺序）。
    let both = env(&[
        ("ZCODE_SESSION_DB_PATH", "/a.sqlite"),
        ("ZCODE_SESSION_DB", "/b.sqlite"),
    ]);
    assert_eq!(
        db(&layers(&user, &projects), &both).as_deref(),
        Some("/b.sqlite")
    );
    let reversed = env(&[
        ("ZCODE_SESSION_DB", "/b.sqlite"),
        ("ZCODE_SESSION_DB_PATH", "/a.sqlite"),
    ]);
    assert_eq!(
        db(&layers(&user, &projects), &reversed).as_deref(),
        Some("/a.sqlite")
    );
    let settings = StorageLayers::from_layers(&layers(&user, &projects), &both);
    assert_eq!(settings.settings().session_db_path, Some("/b.sqlite"));
}
