//! `plugins` 段、插件存储根与插件候选（config.md §2.1、ext.md §4.1）。
#![allow(clippy::disallowed_methods, reason = "测试准备临时目录里的插件清单")]

use rust_cli_ext::plugins::PluginOrigin;
use rust_cli_kernel_api::permission::RuleSource;
use rust_cli_paths::StoragePaths;
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

#[test]
fn the_plugins_section_merges_like_node() {
    let user = object(json!({"plugins": {
        "dirs": ["/Users/dev/a", "rel"],
        "enabledPlugins": {"x@m": true, "y@m": true},
        "suppressedBuiltins": ["s1@zcode-plugins-official"],
        "extraKnownMarketplaces": {"team": {"source": {"source": "github", "repo": "example/plugins"}}},
        "options": {"x@m": {"key": "value"}}
    }}));
    let projects = [
        object(
            json!({"plugins": {"dirs": ["rel", "/Users/dev/b"], "enabledPlugins": {"y@m": false}}}),
        ),
        object(json!({"plugins": {"suppressedBuiltins": ["s2@zcode-plugins-official"]}})),
    ];
    let mut warnings = Vec::new();
    let merged = merge(&layers(&user, &projects), &mut warnings);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(merged.enabled);
    // `dirs` 拼接去重；`enabledPlugins` 按 id 覆盖；`suppressedBuiltins` 由最后写它的层整体替换。
    assert_eq!(merged.dirs, ["/Users/dev/a", "rel", "/Users/dev/b"]);
    assert_eq!(
        merged.enabled_plugins.into_iter().collect::<Vec<_>>(),
        [("x@m".to_owned(), true), ("y@m".to_owned(), false)]
    );
    assert_eq!(merged.suppressed_builtins, ["s2@zcode-plugins-official"]);

    let off = [object(json!({"plugins": {"enabled": false}}))];
    let merged = merge(&layers(&user, &off), &mut warnings);
    assert!(!merged.enabled);
}

#[test]
fn bad_plugin_items_are_skipped_with_warnings() {
    let user = object(json!({"plugins": "all"}));
    let projects = [object(json!({"plugins": {
        "enabled": "yes",
        "dirs": ["ok", "", 3],
        "enabledPlugins": {"a@m": true, "b@m": "no"},
        "suppressedBuiltins": "s"
    }}))];
    let mut warnings = Vec::new();
    let merged = merge(&layers(&user, &projects), &mut warnings);
    assert!(merged.enabled);
    assert_eq!(merged.dirs, ["ok"]);
    assert_eq!(
        merged.enabled_plugins.into_iter().collect::<Vec<_>>(),
        [("a@m".to_owned(), true)]
    );
    assert!(merged.suppressed_builtins.is_empty());
    assert_eq!(warnings.len(), 6, "{warnings:?}");
    assert!(warnings[0].starts_with("user: plugins must be an object"));
}

#[tokio::test]
async fn candidates_follow_the_node_order() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let root = StoragePaths::resolve(&Default::default(), home, home).plugin_storage(None, home);
    let guide = root
        .as_path()
        .join("cache/zcode-plugins-official/zcode-guide/0.3.0");
    let partition = root
        .as_path()
        .join("marketplaces/zcode-plugins-official/bundled-marketplace.json");
    std::fs::create_dir_all(partition.parent().unwrap()).unwrap();
    std::fs::write(
        &partition,
        json!({"version": 1, "manifest": {"plugins": [
            {"name": "zcode-guide", "cachePath": guide.to_string_lossy()}
        ]}})
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        root.as_path().join("installed_plugins.json"),
        json!({"version": 1, "plugins": [{
            "id": "kit@market", "name": "kit", "marketplace": "market", "version": "1.0.0",
            "installPath": "/Users/dev/kit", "installedAt": "2026-10-01T00:00:00.000Z", "scope": "user"
        }]})
        .to_string(),
    )
    .unwrap();
    let section = PluginSection {
        enabled: true,
        dirs: vec!["/Users/dev/abs".to_owned(), "rel/kit".to_owned()],
        enabled_plugins: [("kit@market".to_owned(), true)].into_iter().collect(),
        suppressed_builtins: vec!["zcode-guide@zcode-plugins-official".to_owned()],
    };
    let sources = sources(section.clone(), "/Users/dev/repo", &root).await;
    let candidates: Vec<(String, &str, PluginOrigin)> = sources
        .candidates
        .iter()
        .map(|c| (c.dir.as_str().to_owned(), c.marketplace.as_str(), c.origin))
        .collect();
    assert_eq!(
        candidates,
        [
            (
                "/Users/dev/abs".to_owned(),
                "inline",
                PluginOrigin::ConfigDir
            ),
            (
                "/Users/dev/repo/rel/kit".to_owned(),
                "inline",
                PluginOrigin::ConfigDir
            ),
            (
                guide.to_string_lossy().into_owned(),
                "zcode-plugins-official",
                PluginOrigin::Official
            ),
            (
                "/Users/dev/kit".to_owned(),
                "market",
                PluginOrigin::Installed
            ),
        ]
    );
    assert_eq!(sources.enabled_plugins, section.enabled_plugins);
    assert_eq!(sources.suppressed_builtins, section.suppressed_builtins);

    // `plugins.enabled: false`：配置目录、内置与已安装插件都不加载。
    let off = PluginSection {
        enabled: false,
        ..section
    };
    assert!(sources_of(off, &root).await.candidates.is_empty());
}

async fn sources_of(
    section: PluginSection,
    root: &rust_cli_paths::StoragePath,
) -> rust_cli_ext::plugins::PluginSources {
    sources(section, "/Users/dev/repo", root).await
}

#[test]
fn config_dirs_resolve_like_node_path_resolve() {
    // 绝对路径原样；相对路径相对工作区根，`.` 与 `..` 按字面化简；不展开 `~`（Node `resolve`）。
    assert_eq!(
        resolve_dir("/Users/dev/repo", "/Users/dev/kit"),
        "/Users/dev/kit"
    );
    assert_eq!(
        resolve_dir("/Users/dev/repo", "./kit"),
        "/Users/dev/repo/kit"
    );
    assert_eq!(resolve_dir("/Users/dev/repo", "../kit/"), "/Users/dev/kit");
    assert_eq!(
        resolve_dir("/Users/dev/repo", "~/kit"),
        "/Users/dev/repo/~/kit"
    );
}
