//! 插件来源与启用判定（ext.md §4.3、§4.4）。

use std::collections::BTreeMap;

use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::testing::MemoryEnvironment;

use super::*;
use crate::test_support::{facts, workspace};

const OFFICIAL: &str = "zcode-plugins-official";
const CACHE: &str = "/Users/dev/.zcode/cli/plugins/cache";

fn manifest(dir: &str, body: &str) -> (String, String) {
    (format!("{dir}/.zcode-plugin/plugin.json"), body.to_owned())
}

fn env(files: &[(String, String)]) -> MemoryEnvironment {
    files
        .iter()
        .fold(MemoryEnvironment::new(facts()), |env, (path, text)| {
            env.with_file(path, text)
        })
}

fn candidate(dir: &str, marketplace: &str, origin: PluginOrigin) -> PluginCandidate {
    PluginCandidate {
        dir: EnvPath::new(dir),
        marketplace: marketplace.to_owned(),
        origin,
    }
}

fn official(name: &str) -> PluginCandidate {
    candidate(
        &format!("{CACHE}/{OFFICIAL}/{name}/1.0.0"),
        OFFICIAL,
        PluginOrigin::Official,
    )
}

fn official_manifest(name: &str, extra: &str) -> (String, String) {
    manifest(
        &format!("{CACHE}/{OFFICIAL}/{name}/1.0.0"),
        &format!(r#"{{"name": "{name}"{extra}}}"#),
    )
}

fn enabled(pairs: &[(&str, bool)]) -> BTreeMap<String, bool> {
    pairs
        .iter()
        .map(|(id, on)| ((*id).to_owned(), *on))
        .collect()
}

fn names(loaded: &Loaded) -> Vec<&str> {
    loaded
        .plugins
        .0
        .iter()
        .map(|plugin| plugin.name.as_str())
        .collect()
}

fn reasons(loaded: &Loaded) -> Vec<(Option<&str>, &SkipReason)> {
    loaded
        .skipped
        .iter()
        .map(|skipped| (skipped.id.as_deref(), &skipped.reason))
        .collect()
}

#[tokio::test]
async fn enablement_follows_enabled_plugins_and_the_defaults() {
    let files = [
        manifest("/Users/dev/cfg", r#"{"name": " cfg-kit "}"#),
        (
            "/Users/dev/cfg/skills/hello/SKILL.md".to_owned(),
            "Hello\n".to_owned(),
        ),
        manifest("/Users/dev/off", r#"{"name": "off"}"#),
        manifest("/Users/dev/market-kit", r#"{"name": "market-kit"}"#),
        manifest("/Users/dev/on-kit", r#"{"name": "on-kit"}"#),
        official_manifest("skill-creator", ""),
        official_manifest("restore-legacy-sessions", ""),
        official_manifest("zcode-guide", ""),
    ];
    let sources = PluginSources {
        candidates: vec![
            candidate("/Users/dev/cfg", "inline", PluginOrigin::ConfigDir),
            candidate("/Users/dev/off", "inline", PluginOrigin::ConfigDir),
            official("skill-creator"),
            official("restore-legacy-sessions"),
            official("zcode-guide"),
            candidate("/Users/dev/market-kit", "market", PluginOrigin::Installed),
            candidate("/Users/dev/on-kit", "market", PluginOrigin::Installed),
        ],
        enabled_plugins: enabled(&[("off@inline", false), ("on-kit@market", true)]),
        suppressed_builtins: vec![format!("zcode-guide@{OFFICIAL}")],
    };
    let loaded = load_all(&env(&files), &workspace(), &[], &sources).await;
    // 配置目录缺省启用；官方插件只有缺省名单里的启用；已安装插件要 `enabledPlugins` 打开。
    assert_eq!(names(&loaded), ["cfg-kit", "skill-creator", "on-kit"]);
    assert_eq!(
        loaded.plugins.0[0].skill_dirs,
        ["/Users/dev/cfg/skills".to_owned()]
    );
    assert_eq!(
        reasons(&loaded),
        [
            (Some("off@inline"), &SkipReason::Disabled),
            (
                Some("restore-legacy-sessions@zcode-plugins-official"),
                &SkipReason::Disabled
            ),
            (
                Some("zcode-guide@zcode-plugins-official"),
                &SkipReason::Suppressed
            ),
            (Some("market-kit@market"), &SkipReason::Disabled),
        ]
    );
}

#[tokio::test]
async fn sources_other_than_the_command_line_need_a_valid_manifest() {
    let files = [
        (
            "/Users/dev/bare/skills/x/SKILL.md".to_owned(),
            "B\n".to_owned(),
        ),
        manifest("/Users/dev/bad", r#"{"name": "Bad Name"}"#),
        (
            "/Users/dev/other/.codex-plugin/plugin.json".to_owned(),
            r#"{"name": "other-kit"}"#.to_owned(),
        ),
        (
            "/Users/dev/session/skills/y/SKILL.md".to_owned(),
            "S\n".to_owned(),
        ),
    ];
    let sources = PluginSources {
        candidates: vec![
            candidate("/Users/dev/bare", "inline", PluginOrigin::ConfigDir),
            candidate("/Users/dev/bad", "inline", PluginOrigin::ConfigDir),
            candidate("/Users/dev/other", "inline", PluginOrigin::ConfigDir),
            candidate("/Users/dev/missing", "inline", PluginOrigin::ConfigDir),
        ],
        ..PluginSources::default()
    };
    let session = [EnvPath::new("/Users/dev/session")];
    let loaded = load_all(&env(&files), &workspace(), &session, &sources).await;
    // `--plugin-dir` 没有 manifest 时取目录名；其余来源按 Node 必须有合法的 manifest。
    assert_eq!(names(&loaded), ["session", "other-kit"]);
    let reasons: Vec<&SkipReason> = loaded.skipped.iter().map(|s| &s.reason).collect();
    assert!(matches!(
        reasons.as_slice(),
        [
            SkipReason::ManifestMissing,
            SkipReason::ManifestInvalid(_),
            SkipReason::RootMissing
        ]
    ));
}

#[tokio::test]
async fn session_plugins_and_first_comers_win_name_conflicts() {
    let files = [
        manifest("/Users/dev/session-dup", r#"{"name": "dup"}"#),
        manifest("/Users/dev/cfg-dup", r#"{"name": "dup"}"#),
        manifest("/Users/dev/twin-a", r#"{"name": "twin"}"#),
        manifest("/Users/dev/twin-b", r#"{"name": "twin"}"#),
        manifest("/Users/dev/twin-market", r#"{"name": "twin"}"#),
    ];
    let sources = PluginSources {
        candidates: vec![
            candidate("/Users/dev/cfg-dup", "inline", PluginOrigin::ConfigDir),
            candidate("/Users/dev/twin-a", "inline", PluginOrigin::ConfigDir),
            candidate("/Users/dev/twin-b", "inline", PluginOrigin::ConfigDir),
            candidate("/Users/dev/twin-market", "market", PluginOrigin::Installed),
        ],
        enabled_plugins: enabled(&[("twin@market", true)]),
        ..PluginSources::default()
    };
    let session = [EnvPath::new("/Users/dev/session-dup")];
    let loaded = load_all(&env(&files), &workspace(), &session, &sources).await;
    assert_eq!(names(&loaded), ["dup", "twin"]);
    assert_eq!(loaded.plugins.0[1].dir, "/Users/dev/twin-a");
    assert_eq!(
        reasons(&loaded),
        [
            (Some("dup@inline"), &SkipReason::ShadowedBySession),
            (Some("twin@inline"), &SkipReason::DuplicateId),
            (Some("twin@market"), &SkipReason::DuplicateName),
        ]
    );
}

#[tokio::test]
async fn bundled_plugins_that_need_the_node_runtime_are_skipped() {
    let host_mcp = r#", "mcpServers": {"documents": {"type": "stdio", "command": "/opt/zcode/zcode", "args": ["__zcode-plugin-host", "/x/dist/mcp/server.js"]}}"#;
    let files = [
        official_manifest("browser-use", r#", "skills": "skills""#),
        official_manifest("computer-use", ""),
        official_manifest("documents", host_mcp),
        official_manifest("image-search", ""),
        (
            format!("{CACHE}/{OFFICIAL}/image-search/1.0.0/.mcp.json"),
            r#"{"mcpServers": {"image_search": {"type": "http", "url": "${ZCODE_BASE_URL}/mcp", "auth": {"type": "zcode_official", "provider": "jwt_token"}}}}"#.to_owned(),
        ),
        official_manifest("visualize", ""),
        official_manifest("android-emulator", host_mcp),
        official_manifest("skill-creator", ""),
        manifest(
            "/Users/dev/third",
            &format!(r#"{{"name": "third"{host_mcp}}}"#),
        ),
    ];
    let sources = PluginSources {
        candidates: vec![
            official("browser-use"),
            official("computer-use"),
            official("documents"),
            official("image-search"),
            official("visualize"),
            official("android-emulator"),
            official("skill-creator"),
            candidate("/Users/dev/third", "market", PluginOrigin::Installed),
        ],
        enabled_plugins: enabled(&[
            ("computer-use@zcode-plugins-official", true),
            ("third@market", true),
        ]),
        ..PluginSources::default()
    };
    let loaded = load_all(&env(&files), &workspace(), &[], &sources).await;
    // 只判定官方市场的插件：第三方插件里同样的参数照常加载。
    assert_eq!(names(&loaded), ["skill-creator", "third"]);
    assert_eq!(
        reasons(&loaded),
        [
            (
                Some("browser-use@zcode-plugins-official"),
                &SkipReason::NodeRuntime(NodeDependency::NodeRepl)
            ),
            (
                Some("computer-use@zcode-plugins-official"),
                &SkipReason::NodeRuntime(NodeDependency::NodeRepl)
            ),
            (
                Some("documents@zcode-plugins-official"),
                &SkipReason::NodeRuntime(NodeDependency::PluginHost)
            ),
            (
                Some("image-search@zcode-plugins-official"),
                &SkipReason::NodeRuntime(NodeDependency::OfficialAuth)
            ),
            (
                Some("visualize@zcode-plugins-official"),
                &SkipReason::NodeRuntime(NodeDependency::ViewHost)
            ),
            // 未启用的插件只记“未启用”，不判定 Node 依赖。
            (
                Some("android-emulator@zcode-plugins-official"),
                &SkipReason::Disabled
            ),
        ]
    );
}
