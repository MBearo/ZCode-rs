use super::*;
use serde_json::json;

async fn write(path: &Path, text: &str) {
    tokio::fs::create_dir_all(path.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(path, text).await.unwrap();
}

async fn plugin(root: &Path, manifest: Value) {
    write(
        &root.join(".zcode-plugin/plugin.json"),
        &manifest.to_string(),
    )
    .await;
}

struct Fixture {
    _dir: tempfile::TempDir,
    storage: PathBuf,
    cwd: PathBuf,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let storage = dir.path().join("plugins");
    let cwd = dir.path().join("w");
    // inline：默认启用，带 skill、hooks、MCP 与不支持的组件。
    let inline = cwd.join("tools/demo");
    plugin(
        &inline,
        json!({"name":"demo","version":"1.2.0","description":"Demo","author":{"name":"Z","url":"https://z"},
            "skills":["extra","missing"],"settings":{},
            "hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"echo ${CLAUDE_PLUGIN_ROOT}"}]}],
                "Unknown":[]},
            "mcpServers":{"srv":{"command":"node","args":["${CLAUDE_PLUGIN_ROOT}/s.js"]}}}),
    )
    .await;
    write(
        &inline.join("skills/a/SKILL.md"),
        "---\nname: alpha\ndescription: A\n---",
    )
    .await;
    write(
        &inline.join("extra/SKILL.md"),
        "---\nname: extra-skill\n---",
    )
    .await;
    write(
        &inline.join("commands/run.md"),
        "---\ndescription: Run it\n---",
    )
    .await;
    write(&inline.join("agents/rev.md"), "---\nname: reviewer\n---").await;
    // 官方 bundled：一个默认启用，一个被抑制。
    let official = storage.join("cache/zcode-plugins-official");
    plugin(
        &official.join("skill-creator/0.1.0"),
        json!({"name":"skill-creator"}),
    )
    .await;
    plugin(&official.join("pdf/0.1.0"), json!({"name":"pdf"})).await;
    let partition = json!({"version":1,"manifest":{"name":"zcode-plugins-official","plugins":[
        {"name":"skill-creator","cachePath":official.join("skill-creator/0.1.0")},
        {"name":"pdf","cachePath":official.join("pdf/0.1.0")}]}});
    write(
        &storage.join("marketplaces/zcode-plugins-official/bundled-marketplace.json"),
        &partition.to_string(),
    )
    .await;
    // cache：已安装记录（其中一条根目录缺失）。
    let cached = storage.join("cache/market/tool/2.0.0");
    plugin(&cached, json!({"name":"tool"})).await;
    let installed = json!({"version":1,"plugins":[
        {"id":"tool@market","name":"tool","marketplace":"market","version":"2.0.0",
            "installPath":cached,"installedAt":"t","scope":"user"},
        {"id":"gone@market","name":"gone","marketplace":"market","version":"1.0.0",
            "installPath":storage.join("cache/market/gone/1.0.0"),"installedAt":"t","scope":"user"}]});
    write(
        &storage.join("installed_plugins.json"),
        &installed.to_string(),
    )
    .await;
    Fixture {
        _dir: dir,
        storage,
        cwd,
    }
}

async fn run(fixture: &Fixture, plugins: Value) -> Outcome {
    let config = json!({ "plugins": plugins });
    let env = |_: &str| None;
    let cancel = CancellationToken::new();
    discover(&Request {
        config: &config,
        storage: &fixture.storage,
        cwd: &fixture.cwd,
        env: &env,
        cancel: &cancel,
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn candidates_load_in_node_order_with_their_enabled_state() {
    let fixture = fixture().await;
    let outcome = run(
        &fixture,
        json!({"dirs":["tools/demo"],"enabledPlugins":{"tool@market":true},
            "suppressedBuiltins":["pdf@zcode-plugins-official"],"options":{"demo@inline":{"k":"v"}}}),
    )
    .await;
    let ids: Vec<_> = outcome
        .plugins
        .iter()
        .map(|p| (p.loaded.id.as_str(), p.enabled, p.loaded.source))
        .collect();
    assert_eq!(
        ids,
        vec![
            ("demo@inline", true, Source::Inline),
            (
                "skill-creator@zcode-plugins-official",
                true,
                Source::Official
            ),
            ("tool@market", true, Source::Cache),
        ]
    );
    let demo = &outcome.plugins[0];
    assert_eq!(demo.skill_count, 2);
    // 与 Node 相同：声明路径不做存在性检查，缺失的 missing 也是一个根。
    assert_eq!(demo.skill_root_count, 3, "default skills/, extra, missing");
    assert_eq!(demo.command_root_count, 1);
    assert_eq!(demo.mcp_server_names, vec!["plugin:demo:srv"]);
    assert_eq!(demo.declared_mcp, vec!["srv"]);
    assert_eq!(demo.configured_options["k"], "v");
    assert!(demo.data_path.ends_with("data/demo@inline"));
    assert!(tokio::fs::metadata(&demo.data_path).await.unwrap().is_dir());
    let kinds: Vec<_> = demo.components.iter().map(|g| g.kind).collect();
    assert_eq!(kinds, vec!["agent", "command", "skill", "hook", "mcp"]);
    assert_eq!(demo.components[0].items[0].0, "reviewer");
    assert_eq!(
        demo.components[1].items[0],
        ("run".into(), Some("Run it".into()))
    );
    assert_eq!(demo.hook_details[0]["matcher"], "Bash");
    assert_eq!(outcome.hooks.len(), 1);
    assert_eq!(
        outcome.hooks[0].1["hooks"][0]["plugin"]["id"],
        "demo@inline"
    );
    assert_eq!(
        outcome.mcp_servers[0].1["env"]["ZCODE_PLUGIN_ID"],
        "demo@inline"
    );
    assert!(
        outcome.mcp_servers[0].1["args"][0]
            .as_str()
            .unwrap()
            .ends_with("tools/demo/s.js")
    );
    assert_eq!(outcome.skill_roots.len(), 3);
    let codes: Vec<_> = outcome.diagnostics.iter().map(|d| d.code).collect();
    assert_eq!(
        codes,
        vec![
            "plugin_unsupported_component",
            "plugin_hook_unsupported_event",
            "plugin_skill_root_empty",
            "plugin_root_not_found",
        ]
    );
}

#[tokio::test]
async fn disabled_plugins_keep_components_but_contribute_nothing() {
    let fixture = fixture().await;
    let outcome = run(
        &fixture,
        json!({"dirs":["tools/demo"],"enabledPlugins":{"demo@inline":false,
            "skill-creator@zcode-plugins-official":false}}),
    )
    .await;
    let demo = &outcome.plugins[0];
    assert!(!demo.enabled);
    assert_eq!((demo.skill_count, demo.skill_root_count), (0, 0));
    assert!(demo.mcp_server_names.is_empty());
    assert_eq!(demo.hook_details.len(), 1, "details stay visible");
    assert_eq!(demo.components.len(), 5);
    assert!(outcome.hooks.is_empty() && outcome.mcp_servers.is_empty());
    assert!(outcome.skill_roots.is_empty());
    assert!(!tokio::fs::try_exists(&demo.data_path).await.unwrap());
    let off = run(&fixture, json!({"enabled":false,"dirs":["tools/demo"]})).await;
    assert!(off.plugins.is_empty() && off.diagnostics.is_empty());
    // 同一插件经两个 inline 目录出现：后者作为重复项忽略。
    let twice = run(
        &fixture,
        json!({"dirs":["tools/demo","tools/../tools/demo"]}),
    )
    .await;
    assert!(
        twice
            .diagnostics
            .iter()
            .any(|d| d.code == "plugin_duplicate_id")
    );
}
