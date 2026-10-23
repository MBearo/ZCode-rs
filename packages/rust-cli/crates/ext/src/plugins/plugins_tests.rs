//! 插件目录与 manifest（ext.md §4）。

use rust_cli_kernel_api::mcp::McpTransportConfig;
use rust_cli_kernel_api::testing::MemoryEnvironment;

use super::*;
use crate::test_support::{facts, workspace};

#[tokio::test]
async fn manifest_components_and_mcp_servers() {
    let root = "/Users/dev/plugins/kit";
    let env = MemoryEnvironment::new(facts())
        .with_file(&format!("{root}/.claude-plugin/plugin.json"), r#"{"name": "toolkit", "skills": "./extra", "agents": ["./agents/reviewer.md", "./more.md"], "hooks": {"Stop": []}, "mcpServers": {"db": {"command": "${CLAUDE_PLUGIN_ROOT}/bin/db", "args": ["--root", "${ZCODE_PLUGIN_ROOT}"]}}}"#)
        .with_file(&format!("{root}/skills/deploy/SKILL.md"), "---\ndescription: Deploy\n---\n")
        .with_file(&format!("{root}/extra/lint/SKILL.md"), "---\ndescription: Lint\n---\n")
        .with_file(&format!("{root}/agents/reviewer.md"), "---\nname: reviewer\n---\n")
        .with_file(&format!("{root}/agents/team/lead.md"), "Lead\n")
        .with_file(&format!("{root}/more.md"), "More\n")
        .with_file(&format!("{root}/hooks/hooks.json"), r#"{"hooks": {"Stop": []}}"#)
        .with_file(&format!("{root}/.mcp.json"), r#"{"mcpServers": {"web": {"type": "http", "url": "https://mcp.example.com"}}}"#)
        .with_file("/Users/dev/plugins/bare/skills/x/SKILL.md", "B\n");
    let dirs = [
        EnvPath::new(root),
        EnvPath::new("/Users/dev/plugins/bare"),
        EnvPath::new("/Users/dev/plugins/missing"),
    ];
    let plugins = load_all(&env, &workspace(), &dirs, &PluginSources::default())
        .await
        .plugins;
    assert_eq!(plugins.0.len(), 2);
    let kit = &plugins.0[0];
    assert_eq!(kit.name, "toolkit");
    assert_eq!(
        kit.skill_dirs,
        [format!("{root}/skills"), format!("{root}/extra")]
    );
    // 子目录是命名空间；manifest 再次列出的同一文件只取一次。
    let agents: Vec<(String, Vec<String>)> = kit
        .agent_files
        .iter()
        .map(|file| (file.path.clone(), file.namespace.clone()))
        .collect();
    assert_eq!(
        agents,
        [
            (format!("{root}/agents/reviewer.md"), Vec::new()),
            (
                format!("{root}/agents/team/lead.md"),
                vec!["team".to_owned()]
            ),
            (format!("{root}/more.md"), Vec::new()),
        ]
    );
    // 内联的 manifest hooks 包成文件的 `{"hooks": …}` 形状。
    assert_eq!(
        kit.hooks,
        [
            serde_json::json!({"hooks": {"Stop": []}}),
            serde_json::json!({"hooks": {"Stop": []}})
        ]
    );
    let names: Vec<&str> = kit
        .mcp_servers
        .iter()
        .map(|server| server.name.as_str())
        .collect();
    assert_eq!(names, ["plugin:toolkit:web", "plugin:toolkit:db"]);
    let McpTransportConfig::Stdio { command, args, .. } = &kit.mcp_servers[1].transport else {
        panic!("db is stdio");
    };
    assert_eq!(command, &format!("{root}/bin/db"));
    assert_eq!(args, &["--root".to_owned(), root.to_owned()]);
    assert_eq!(plugins.0[1].name, "bare");
    plugin_stdio_servers_get_the_node_variables(kit, root).await;
    let roots: Vec<String> = plugins
        .skill_roots()
        .iter()
        .map(|root| format!("{}:{}", root.plugin, root.dir))
        .collect();
    assert_eq!(
        roots,
        [
            format!("toolkit:{root}/skills"),
            format!("toolkit:{root}/extra"),
            "bare:/Users/dev/plugins/bare/skills".to_owned()
        ]
    );
}

/// 插件的 stdio MCP 服务器的环境（修复线 F42，config.md §2.5.1）：按 Node（`adapters/src/plugins/mcp.ts`）给插件目录与
/// 项目目录，两套名字；HTTP 服务器没有环境。
async fn plugin_stdio_servers_get_the_node_variables(kit: &Plugin, root: &str) {
    let McpTransportConfig::Stdio { env, .. } = &kit.mcp_servers[1].transport else {
        panic!("db is stdio");
    };
    let project = workspace().root.as_str().to_owned();
    for (name, value) in [
        ("CLAUDE_PLUGIN_ROOT", root),
        ("ZCODE_PLUGIN_ROOT", root),
        ("CLAUDE_PROJECT_DIR", project.as_str()),
        ("ZCODE_PROJECT_DIR", project.as_str()),
    ] {
        assert_eq!(env.get(name).map(String::as_str), Some(value), "{name}");
    }
}

/// 配置里的 `env` 优先（Node 先放缺省再展开配置的 `env`）。
#[tokio::test]
async fn configured_env_wins_over_the_provided_variables() {
    let root = "/Users/dev/plugins/own";
    let env = MemoryEnvironment::new(facts()).with_file(
        &format!("{root}/.claude-plugin/plugin.json"),
        r#"{"name": "own", "mcpServers": {"db": {"command": "db", "env": {"CLAUDE_PROJECT_DIR": "/elsewhere", "TOKEN": "x"}}}}"#,
    );
    let plugins = load_all(
        &env,
        &workspace(),
        &[EnvPath::new(root)],
        &PluginSources::default(),
    )
    .await
    .plugins;
    let McpTransportConfig::Stdio { env, .. } = &plugins.0[0].mcp_servers[0].transport else {
        panic!("db is stdio");
    };
    assert_eq!(env["CLAUDE_PROJECT_DIR"], "/elsewhere");
    assert_eq!(env["TOKEN"], "x");
    assert_eq!(env["ZCODE_PLUGIN_ROOT"], root);
}
