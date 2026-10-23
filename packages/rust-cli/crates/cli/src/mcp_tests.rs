//! MCP 设置的组装（ext.md §3.1）：配置文件的服务器加 `--mcp-config`、`--strict-mcp-config` 与环境变量。

use rust_cli_kernel_api::env::EnvFacts;
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::mcp::McpTransportConfig;
use rust_cli_kernel_api::testing::MemoryEnvironment;

use super::*;

const CWD: &str = "/Users/dev/project";

fn workspace() -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(CWD),
    }
}

fn env() -> MemoryEnvironment {
    let facts = EnvFacts {
        cwd: EnvPath::new(CWD),
        platform: "linux".to_owned(),
        os_version: "Linux".to_owned(),
        shell: "bash".to_owned(),
        is_git_repo: false,
        additional_directories: Vec::new(),
        home: Some(EnvPath::new("/Users/dev")),
    };
    MemoryEnvironment::new(facts)
        .with_file(
            "/Users/dev/project/flags.json",
            r#"{"mcpServers": {"cli": {"command": "x"}, "shared": {"command": "${TOOL:-flag}"}}}"#,
        )
        // 旧位置不再读（ext.md §3.1）。
        .with_file(
            "/Users/dev/project/.mcp.json",
            r#"{"mcpServers": {"old": {"command": "x"}}}"#,
        )
        .with_file(
            "/Users/dev/.zcode.json",
            r#"{"mcpServers": {"old-user": {"command": "x"}}}"#,
        )
}

fn configured() -> Vec<McpServer> {
    ["shared", "user"]
        .into_iter()
        .map(|name| McpServer {
            name: name.to_owned(),
            scope: McpScope::User,
            transport: McpTransportConfig::Stdio {
                command: "configured".to_owned(),
                args: Vec::new(),
                env: Default::default(),
                cwd: None,
            },
            options: Default::default(),
        })
        .collect()
}

fn names(settings: &McpSettings) -> Vec<String> {
    settings
        .servers
        .iter()
        .map(|server| format!("{}:{:?}", server.name, server.scope))
        .collect()
}

#[tokio::test]
async fn flags_override_configured_servers_by_name() {
    let flags = McpFlags {
        configs: vec!["flags.json".to_owned()],
        ..McpFlags::default()
    };
    let snapshot = vec![("MCP_TIMEOUT".to_owned(), "5000".to_owned())];
    let settings = settings(
        &env(),
        &workspace(),
        (&flags, &[]),
        &snapshot,
        configured(),
        true,
    )
    .await;
    assert_eq!(
        names(&settings),
        ["cli:Dynamic", "shared:Dynamic", "user:User"]
    );
    // `--mcp-config` 仍展开 `${VAR:-默认}`。
    assert!(matches!(
        &settings.servers[1].transport,
        McpTransportConfig::Stdio { command, .. } if command == "flag"
    ));
    assert_eq!(settings.connect_timeout_ms, 5000);
    assert_eq!(settings.max_output_tokens, 25_000);
}

#[tokio::test]
async fn strict_uses_only_flags_and_features_off_disables_everything() {
    let strict = McpFlags {
        configs: vec![r#"{"mcpServers": {"only": {"command": "x"}}}"#.to_owned()],
        strict: true,
        ..McpFlags::default()
    };
    let only = settings(
        &env(),
        &workspace(),
        (&strict, &[]),
        &[],
        configured(),
        true,
    )
    .await;
    assert_eq!(names(&only), ["only:Dynamic"]);
    let off = settings_off(&strict).await;
    assert!(off.servers.is_empty());
    let empty = settings(
        &env(),
        &workspace(),
        (&McpFlags::default(), &[]),
        &[],
        Vec::new(),
        true,
    )
    .await;
    assert!(empty.servers.is_empty());
}

async fn settings_off(flags: &McpFlags) -> McpSettings {
    settings(&env(), &workspace(), (flags, &[]), &[], configured(), false).await
}

fn vars(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect()
}

/// 整数环境变量的读法（修复线 F36）。
#[test]
fn integers_are_parsed_like_js_numbers() {
    assert_eq!(env_int("2000"), Some(2000));
    assert_eq!(env_int(" 2000ms "), Some(2000));
    assert_eq!(env_int("1e3"), Some(1000));
    assert_eq!(env_int("1.e3"), Some(1000));
    assert_eq!(env_int(".5e1"), Some(5));
    assert_eq!(env_int("1.5e0"), None);
    assert_eq!(env_int("1,500"), Some(1500));
    assert_eq!(env_int("1_500_000"), Some(1_500_000));
    assert_eq!(env_int("1,500_000"), Some(1));
    assert_eq!(env_int("-5"), Some(-5));
    assert_eq!(env_int("+7"), Some(7));
    assert_eq!(env_int("0x10"), Some(0));
    assert_eq!(env_int("abc"), None);
    assert_eq!(env_int(""), None);
}

/// `-p` 的启动等待（ext.md §3.3.1，修复线 F36）：每个变量的缺省、读法与优先级。
#[test]
fn the_startup_wait_follows_the_variables() {
    let none = startup(&[], (false, &McpFlags::default()), 30_000, &[]);
    assert!(none.nonblocking);
    assert_eq!(none.connect_wait_ms, 5_000);
    assert_eq!(none.prewait_ms, 2_000);
    // MCP_CONNECTION_NONBLOCKING：只有“假”时阻塞。
    for (value, nonblocking) in [
        ("false", false),
        (" OFF ", false),
        ("0", false),
        ("true", true),
        ("maybe", true),
    ] {
        let found = startup(
            &vars(&[("MCP_CONNECTION_NONBLOCKING", value)]),
            (false, &McpFlags::default()),
            30_000,
            &[],
        );
        assert_eq!(found.nonblocking, nonblocking, "{value}");
    }
    // MCP_CONNECT_TIMEOUT_MS：正整数，否则缺省；上限 2147483647。
    let wait = |value: &str| {
        startup(
            &vars(&[("MCP_CONNECT_TIMEOUT_MS", value)]),
            (false, &McpFlags::default()),
            30_000,
            &[],
        )
        .connect_wait_ms
    };
    assert_eq!(wait("1500"), 1_500);
    assert_eq!(wait("0"), 5_000);
    assert_eq!(wait("-3"), 5_000);
    assert_eq!(wait("99999999999"), 2_147_483_647);
    // prewait：ZCODE_MCP_STARTUP_WAIT_MS > 显式 --mcp-config 的 MCP_TIMEOUT > 2000。
    let prewait = |pairs: &[(&str, &str)], explicit: bool| {
        startup(&vars(pairs), (explicit, &McpFlags::default()), 4_000, &[]).prewait_ms
    };
    assert_eq!(prewait(&[], false), 2_000);
    assert_eq!(prewait(&[], true), 4_000);
    assert_eq!(prewait(&[("ZCODE_MCP_STARTUP_WAIT_MS", "750")], true), 750);
    assert_eq!(prewait(&[("ZCODE_MCP_STARTUP_WAIT_MS", "0")], true), 0);
    // 负数不合法（`int({min: 0})`）：当作没设。
    assert_eq!(
        prewait(&[("ZCODE_MCP_STARTUP_WAIT_MS", "-1")], false),
        2_000
    );
    // allow 规则点名的服务器。
    let allowed = ["mcp__slow".to_owned(), "Bash(sleep:*)".to_owned()];
    let named = startup(&[], (false, &McpFlags::default()), 30_000, &allowed);
    assert_eq!(named.required.into_iter().collect::<Vec<_>>(), ["slow"]);
}

/// stream-json 输入且模型能用工具搜索时是 `lifted`（headless.md §6，修复线 F39）：② 不等可延迟的服务器；
/// 单次 `-p`、显式 `--mcp-config`、设了等待时间时照常等。
#[test]
fn streaming_input_lifts_the_prewait() {
    let streaming = McpFlags {
        streaming_input: true,
        tool_search: true,
        ..McpFlags::default()
    };
    assert!(!startup(&[], (false, &streaming), 30_000, &[]).wait_for_deferrable);
    assert!(startup(&[], (true, &streaming), 30_000, &[]).wait_for_deferrable);
    // 设了启动等待（`ZCODE_MCP_STARTUP_WAIT_MS`）就不 lifted。
    let waited = vars(&[("ZCODE_MCP_STARTUP_WAIT_MS", "500")]);
    assert!(startup(&waited, (false, &streaming), 30_000, &[]).wait_for_deferrable);
    let no_search = McpFlags {
        tool_search: false,
        ..streaming
    };
    assert!(startup(&[], (false, &no_search), 30_000, &[]).wait_for_deferrable);
    assert!(startup(&[], (false, &McpFlags::default()), 30_000, &[]).wait_for_deferrable);
}

#[tokio::test]
async fn an_explicit_mcp_config_waits_up_to_mcp_timeout() {
    let snapshot = vars(&[("MCP_TIMEOUT", "4000")]);
    let flags = McpFlags {
        configs: vec!["flags.json".to_owned()],
        ..McpFlags::default()
    };
    let explicit = settings(
        &env(),
        &workspace(),
        (&flags, &[]),
        &snapshot,
        configured(),
        true,
    )
    .await;
    assert_eq!(explicit.connect_timeout_ms, 4_000);
    assert_eq!(explicit.startup.prewait_ms, 4_000);
    // 只有配置文件的服务器：2000。
    let configured_only = settings(
        &env(),
        &workspace(),
        (&McpFlags::default(), &[]),
        &snapshot,
        configured(),
        true,
    )
    .await;
    assert_eq!(configured_only.startup.prewait_ms, 2_000);
    // `--mcp-config` 没有给出服务器：不算显式。
    let empty = McpFlags {
        configs: vec![r#"{"mcpServers": {}}"#.to_owned()],
        strict: true,
        ..McpFlags::default()
    };
    let none = settings(
        &env(),
        &workspace(),
        (&empty, &[]),
        &snapshot,
        configured(),
        true,
    )
    .await;
    assert_eq!(none.startup.prewait_ms, 2_000);
}
