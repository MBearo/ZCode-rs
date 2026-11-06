//! 配置的组装（config.md §2、§3）：临时 HOME 里的用户级配置，内存执行环境里的项目文件。
#![allow(clippy::disallowed_methods, reason = "测试准备临时 HOME 里的配置文件")]

use rust_cli_hooks::config::HookCommand;
use rust_cli_kernel_api::env::{EnvFacts, EnvPath};
use rust_cli_kernel_api::hooks::HookEventName;
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::permission::PermissionRule;
use rust_cli_kernel_api::store::SessionStore;
use rust_cli_kernel_api::testing::MemoryEnvironment;
use serde_json::json;

use super::*;

const ROOT: &str = "/Users/dev/repo";

fn env(files: &[(&str, Value)]) -> MemoryEnvironment {
    let facts = EnvFacts {
        cwd: EnvPath::new(ROOT),
        platform: "linux".to_owned(),
        os_version: "Linux".to_owned(),
        shell: "bash".to_owned(),
        is_git_repo: true,
        home: Some(EnvPath::new("/Users/dev")),
        uid: None,
    };
    files
        .iter()
        .fold(MemoryEnvironment::new(facts), |env, (path, value)| {
            env.with_file(path, &value.to_string())
        })
        .with_file("/Users/dev/repo/.git/HEAD", "ref")
}

fn home(user: Option<Value>) -> (tempfile::TempDir, StoragePaths) {
    let dir = tempfile::tempdir().unwrap();
    if let Some(user) = user {
        let file = dir.path().join(".zcode/cli/config.json");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, user.to_string()).unwrap();
    }
    let paths = StoragePaths::resolve(&Default::default(), dir.path(), dir.path());
    (dir, paths)
}

async fn assemble_with(user: Option<Value>, files: &[(&str, Value)]) -> ConfigSnapshot {
    assemble_on(user, files, true).await
}

/// `project_is_local` 为假：项目文件在别的机器上（主 env 不在本进程里）。
async fn assemble_on(
    user: Option<Value>,
    files: &[(&str, Value)],
    project_is_local: bool,
) -> ConfigSnapshot {
    let (_dir, paths) = home(user);
    let env = env(files);
    let workspace = WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(ROOT),
    };
    assemble(&ConfigInputs {
        files: &env,
        workspace: &workspace,
        paths: &paths,
        process_env: &rust_cli_base::secret::EnvSnapshot::default(),
        flag: None,
        project_is_local,
    })
    .await
}

/// 主 env 不在本机时，项目级配置是执行侧的文件，不能改 controller 的存储位置（deployment.md §21）：只认用户级配置。
#[tokio::test]
async fn project_files_on_another_machine_do_not_move_the_storage() {
    let user = json!({"storage": {"dir": "/Users/dev/store"}});
    let project = json!({"storage": {"dir": "/srv/store", "sessionDbPath": "/srv/db.sqlite"}});
    let files = [("/Users/dev/repo/zcode.json", project)];
    let local = assemble_on(Some(user.clone()), &files, true).await.storage;
    assert_eq!(local.dir.as_deref(), Some("/srv/store"));
    assert_eq!(local.session_db_path.as_deref(), Some("/srv/db.sqlite"));
    let remote = assemble_on(Some(user), &files, false).await.storage;
    assert_eq!(remote.dir.as_deref(), Some("/Users/dev/store"));
    assert_eq!(remote.session_db_path, None);
}

#[tokio::test]
async fn no_files_give_an_empty_snapshot() {
    let snapshot = assemble_with(None, &[]).await;
    assert_eq!(snapshot.version, 1);
    assert!(snapshot.rules.is_empty());
    assert_eq!(snapshot.mode, None);
    assert!(snapshot.mcp_servers.is_empty());
    assert!(snapshot.mcp_enabled);
    assert!(
        snapshot
            .hooks
            .sources
            .iter()
            .all(|(_, s)| s.hooks.is_empty())
    );
}

#[tokio::test]
async fn the_three_sections_come_from_the_node_files() {
    let user = json!({
        "permission": {"mode": "plan", "allowedTools": ["Read"], "disallowedTools": ["WebFetch"]},
        "hooks": {"enabled": true, "events": {"Stop": [{"hooks": [{"type": "command", "command": "done.sh"}]}]}},
        "mcp": {"servers": {"user-mcp": {"command": "node"}}}
    });
    let snapshot = assemble_with(
        Some(user),
        &[(
            "/Users/dev/repo/.zcode/config.json",
            json!({
                "permission": {"allowedTools": ["Bash(git status:*)"]},
                "hooks": {"enabled": true, "events": {"Stop": [{"hooks": [{"type": "command", "command": "project.sh"}]}]}},
                "mcp": {"servers": {"repo-mcp": {"type": "http", "url": "https://mcp.example.com"}}}
            }),
        )],
    )
    .await;
    assert_eq!(snapshot.mode, Some(PermissionMode::Plan));
    let rules: Vec<_> = snapshot
        .rules
        .iter()
        .map(|rule| (rule.source, rule.behavior, rule.rule.clone()))
        .collect();
    assert_eq!(
        rules,
        [
            (
                RuleSource::ProjectSettings,
                RuleBehavior::Allow,
                PermissionRule {
                    tool: "Bash".to_owned(),
                    content: Some("git status:*".to_owned()),
                },
            ),
            (
                RuleSource::UserSettings,
                RuleBehavior::Deny,
                PermissionRule {
                    tool: "WebFetch".to_owned(),
                    content: None,
                },
            ),
        ]
    );
    let names: Vec<_> = snapshot
        .mcp_servers
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(names, ["repo-mcp", "user-mcp"]);
    // 用户级 hooks 进快照；项目级 hooks 要先经信任（config.md §4），这里不出现。
    let (source, settings) = &snapshot.hooks.sources[0];
    assert_eq!(*source, HookSource::User);
    let (event, matchers) = &settings.hooks[0];
    assert_eq!(*event, HookEventName::Stop);
    assert!(matches!(&matchers[0].hooks[0], HookCommand::Command(c) if c.command == "done.sh"));
    assert_eq!(snapshot.hooks.sources.len(), 1);
    let pending = snapshot.pending.unwrap();
    assert_eq!(pending.identity, ROOT);
    assert_eq!(pending.digests.len(), 1);
}

#[tokio::test]
async fn rules_are_combined_with_local_settings_and_the_command_line() {
    let snapshot =
        assemble_with(Some(json!({"permission": {"allowedTools": ["Read"]}})), &[]).await;
    let local = vec![SourcedRule {
        source: RuleSource::LocalSettings,
        behavior: RuleBehavior::Ask,
        rule: parse_rule("Bash(rm:*)"),
    }];
    let cli = PermissionRules {
        version: 7,
        rules: sourced(
            RuleSource::CliArg,
            RuleBehavior::Deny,
            &["Write".to_owned()],
        ),
        avoid_prompts: false,
        ..PermissionRules::default()
    };
    let combined = snapshot.permission_rules(local, cli);
    let sources: Vec<_> = combined.rules.iter().map(|rule| rule.source).collect();
    assert_eq!(
        sources,
        [
            RuleSource::UserSettings,
            RuleSource::LocalSettings,
            RuleSource::CliArg
        ]
    );
    assert_eq!(combined.version, 1);
}

/// 新会话的模式（config.md §2.2）：命令行 → 本机项目的持久模式 → 配置文件 → default；命令行给了时不读
/// 持久模式（Node `options.runtimeConfig.mode ?? persistedMode ?? config.permission.mode`）。
#[tokio::test]
async fn the_initial_mode_follows_the_node_order() {
    use rust_cli_kernel_api::testing::MemoryStore;
    let dir = tempfile::tempdir().unwrap();
    let paths = StoragePaths::resolve(&Default::default(), dir.path(), dir.path());
    let store = rust_cli_node_store::NodeStore::new(
        paths,
        std::sync::Arc::new(rust_cli_node_store::NodeIds::new(std::sync::Arc::new(
            rust_cli_base::clock::SystemClock::utc(),
        ))),
    );
    let workspace = WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(ROOT),
    };
    let yolo = PermissionMode::BypassPermissions;
    // 库里还没有持久模式：配置 → default。
    assert_eq!(
        initial_mode(None, Some(yolo), &store, &workspace).await,
        yolo
    );
    assert_eq!(
        initial_mode(None, None, &store, &workspace).await,
        PermissionMode::Default
    );
    // 有持久模式：盖过配置，命令行又盖过它。
    let meta = rust_cli_kernel_api::store::SessionMeta {
        id: rust_cli_kernel_api::ids::SessionId::new("sess_mode"),
        kind: rust_cli_kernel_api::store::SessionKind::Interactive,
        parent: None,
        workspace: workspace.clone(),
        title: String::new(),
        trace: rust_cli_kernel_api::ids::TraceId::new("sess_mode"),
        created_at_ms: 0,
    };
    store.create(meta.clone()).await.unwrap();
    let batch = rust_cli_kernel_api::store::FactBatch {
        owner_epoch: rust_cli_kernel_api::ids::OwnerEpoch::LOCAL,
        command: None,
        facts: vec![rust_cli_kernel_api::store::Fact::ProjectModeSet {
            mode: PermissionMode::AcceptEdits,
        }],
        turn: None,
    };
    store.commit(&meta.id, batch).await.unwrap();
    assert_eq!(
        initial_mode(None, Some(yolo), &store, &workspace).await,
        PermissionMode::AcceptEdits
    );
    assert_eq!(
        initial_mode(Some(PermissionMode::Plan), Some(yolo), &store, &workspace).await,
        PermissionMode::Plan
    );
    // 读不出（不支持的存储）时当作没有。
    assert_eq!(
        initial_mode(None, Some(yolo), &MemoryStore::default(), &workspace).await,
        yolo
    );
}

/// 工作区信任对话框里的项目授权（workspace-trust.md §2.4）与后备询问拒绝后去掉的规则（§2.5）。
#[tokio::test]
async fn the_effective_project_allow_list_feeds_the_trust_dialog() {
    let user = json!({"permission": {"allowedTools": ["Read"], "disallowedTools": ["WebFetch"]}});
    let snapshot = assemble_with(Some(user.clone()), &[]).await;
    assert_eq!(snapshot.project_allow, None);
    let files = [
        (
            "/Users/dev/repo/zcode.json",
            json!({"permission": {"allowedTools": ["Bash(ls:*)"]}}),
        ),
        (
            "/Users/dev/repo/.zcode/config.json",
            json!({"permission": {"allowedTools": ["Edit", 3]}}),
        ),
    ];
    // 离工作区根近的那份不合法时，用前一份（与合并的按键覆盖同一层）。
    let mut snapshot = assemble_with(Some(user), &files).await;
    assert_eq!(
        snapshot.project_allow,
        Some(("zcode.json".to_owned(), vec!["Bash(ls:*)".to_owned()]))
    );
    snapshot.without_project_grants();
    let left: Vec<_> = snapshot
        .rules
        .iter()
        .map(|rule| (rule.source, rule.behavior, rule.rule.tool.as_str()))
        .collect();
    // 项目级的 allow 去掉；用户级的 deny 留下（用户级的 allow 被项目级按键覆盖了）。
    assert_eq!(
        left,
        [(RuleSource::UserSettings, RuleBehavior::Deny, "WebFetch")]
    );
}

/// 设置的 Permissions 段打开的文件：最后写那个键的一层（tui-manage.md §1.2）。
#[tokio::test]
async fn rule_files_name_the_layer_that_last_wrote_each_key() {
    let user = json!({"permission": {"allowedTools": ["Read"], "disallowedTools": ["WebFetch"]}});
    let files = [(
        "/Users/dev/repo/.zcode/config.json",
        json!({"permission": {"allowedTools": ["Bash(ls:*)"]}}),
    )];
    let snapshot = assemble_with(Some(user), &files).await;
    let shown: Vec<_> = snapshot
        .rule_files
        .iter()
        .map(|(source, behavior, path)| {
            (
                *source,
                *behavior,
                path.ends_with("cli/config.json"),
                path.clone(),
            )
        })
        .collect();
    assert_eq!(shown.len(), 2);
    assert_eq!(
        (shown[0].0, shown[0].1, shown[0].3.as_str()),
        (
            RuleSource::ProjectSettings,
            RuleBehavior::Allow,
            "/Users/dev/repo/.zcode/config.json"
        )
    );
    assert_eq!(
        (shown[1].0, shown[1].1, shown[1].2),
        (RuleSource::UserSettings, RuleBehavior::Deny, true)
    );
}
