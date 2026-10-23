//! `/permissions` 的列表组成与修改后的快照（tui-manage.md §1.1、§1.4）。
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, SessionId};
use std::sync::Arc;

use rust_cli_kernel_api::orchestration::PermissionGate;
use rust_cli_kernel_api::permission::RuleBehavior;
use rust_cli_kernel_api::testing::MemoryStore;

use super::*;

fn workspace() -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new("/Users/dev/project"),
    }
}

fn sourced(source: RuleSource, behavior: RuleBehavior, text: &str) -> SourcedRule {
    SourcedRule {
        source,
        behavior,
        rule: parse_rule(text),
    }
}

fn gate() -> PolicyGate {
    PolicyGate::new(PermissionRules {
        version: 3,
        rules: vec![
            sourced(RuleSource::UserSettings, RuleBehavior::Deny, "WebFetch"),
            sourced(RuleSource::LocalSettings, RuleBehavior::Allow, "Glob"),
            sourced(
                RuleSource::CliArg,
                RuleBehavior::Allow,
                "Bash(git status:*)",
            ),
        ],
        ..PermissionRules::default()
    })
}

fn texts(rules: &[SourcedRule]) -> Vec<(RuleSource, RuleBehavior, String)> {
    rules
        .iter()
        .map(|rule| (rule.source, rule.behavior, format_rule(&rule.rule)))
        .collect()
}

async fn add(store: &MemoryStore, behavior: RuleBehavior, text: &str) {
    store
        .change_project_rules(
            &workspace(),
            ProjectRulesChange {
                behavior,
                add: vec![parse_rule(text)],
                remove: Vec::new(),
            },
        )
        .await
        .unwrap();
}

/// 列表：LocalSettings 从 db 现读（快照里的 Glob 是启动时的旧值），会话规则来自快照（进程里共用，R1-2 之后
/// 没有按会话的副本，也不用去重）。
#[tokio::test]
async fn the_listing_reads_local_settings_fresh() {
    let gate = PolicyGate::new(PermissionRules {
        version: 3,
        rules: vec![
            sourced(RuleSource::UserSettings, RuleBehavior::Deny, "WebFetch"),
            sourced(RuleSource::LocalSettings, RuleBehavior::Allow, "Glob"),
            sourced(RuleSource::Session, RuleBehavior::Allow, "Edit"),
        ],
        ..PermissionRules::default()
    });
    let store = MemoryStore::default();
    add(&store, RuleBehavior::Allow, "Read").await;
    let query = PermissionRulesQuery {
        workspace: workspace(),
        session: Some(SessionId::new("ses_1")),
    };
    let rules = listing(&gate, &store, &query).await.unwrap();
    assert_eq!(
        texts(&rules),
        [
            (
                RuleSource::UserSettings,
                RuleBehavior::Deny,
                "WebFetch".to_owned()
            ),
            (
                RuleSource::LocalSettings,
                RuleBehavior::Allow,
                "Read".to_owned()
            ),
            (RuleSource::Session, RuleBehavior::Allow, "Edit".to_owned()),
        ]
    );
}

/// “总是允许”落库后（db 里已有规则），判决链读回同一份规则集：快照的 LocalSettings 层换成库值（R1-2）。
#[tokio::test]
async fn a_committed_always_allow_replaces_the_local_layer() {
    let gate = Arc::new(gate());
    let store = Arc::new(MemoryStore::default());
    add(&store, RuleBehavior::Allow, "Bash(npm test:*)").await;
    let shared = SharedRules {
        gate: gate.clone(),
        store: store.clone(),
    };
    shared.local_rules_committed(&workspace()).await;
    let snapshot = gate.rules();
    assert_eq!(snapshot.version, 4);
    assert_eq!(
        texts(&snapshot.rules),
        [
            (
                RuleSource::UserSettings,
                RuleBehavior::Deny,
                "WebFetch".to_owned()
            ),
            (
                RuleSource::LocalSettings,
                RuleBehavior::Allow,
                "Bash(npm test:*)".to_owned()
            ),
            (
                RuleSource::CliArg,
                RuleBehavior::Allow,
                "Bash(git status:*)".to_owned()
            ),
        ]
    );
}

#[tokio::test]
async fn changes_go_to_the_store_and_replace_the_snapshot() {
    let gate = gate();
    let store = MemoryStore::default();
    let session = SessionId::new("ses_1");
    let request = |change| PermissionRulesChange {
        workspace: workspace(),
        session: Some(session.clone()),
        change,
    };
    let rules = change(
        &gate,
        &store,
        request(RuleChange::Add {
            behavior: RuleBehavior::Allow,
            rules: vec![" Bash(rm *) ".to_owned()],
        }),
    )
    .await
    .unwrap();
    // 快照里的 LocalSettings 换成落库后的规则集（旧的 Glob 不在 db 里，被换掉），版本加一。
    let snapshot = gate.rules();
    assert_eq!(snapshot.version, 4);
    assert_eq!(
        texts(&snapshot.rules),
        [
            (
                RuleSource::UserSettings,
                RuleBehavior::Deny,
                "WebFetch".to_owned()
            ),
            (
                RuleSource::LocalSettings,
                RuleBehavior::Allow,
                "Bash(rm *)".to_owned()
            ),
            (
                RuleSource::CliArg,
                RuleBehavior::Allow,
                "Bash(git status:*)".to_owned()
            ),
        ]
    );
    assert_eq!(rules.len(), 3);
    // 删除 LocalSettings 的允许规则：db 与快照里都没有了。
    let local = sourced(RuleSource::LocalSettings, RuleBehavior::Allow, "Bash(rm *)");
    change(&gate, &store, request(RuleChange::Remove { rule: local }))
        .await
        .unwrap();
    assert!(
        store
            .project_rules(&workspace())
            .await
            .unwrap()
            .allow
            .is_empty()
    );
    assert_eq!(gate.rules().rules.len(), 2);
    // 命令行规则只在内存里去掉；配置文件规则只读。
    let cli = sourced(
        RuleSource::CliArg,
        RuleBehavior::Allow,
        "Bash(git status:*)",
    );
    let rules = change(&gate, &store, request(RuleChange::Remove { rule: cli }))
        .await
        .unwrap();
    assert_eq!(rules.len(), 1);
    let user = sourced(RuleSource::UserSettings, RuleBehavior::Deny, "WebFetch");
    let refused = change(&gate, &store, request(RuleChange::Remove { rule: user }))
        .await
        .unwrap_err();
    assert!(refused.to_string().contains("read-only"), "{refused}");
    assert_eq!(gate.rules().rules.len(), 1);
}
