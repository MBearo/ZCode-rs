//! 项目 hooks 的信任（config.md §4、§5）：digest 与 Node 对拍、准入与 `-p` 的提示。
use rust_cli_hooks::config::HookCommand;
use rust_cli_kernel_api::hooks::HookEventName;
use rust_cli_platform::{ConfigFile, TrustRecord};
use serde_json::json;

use super::super::files::project_file;
use super::*;

const FIXTURE: &str = include_str!("../../tests/fixtures/workspace-hooks.json");
const GOLDEN: &str = include_str!("../../tests/fixtures/workspace-hooks-golden.json");

fn file_with(root: &str, path: &str, order: usize, hooks: &Value) -> ProjectFile {
    let mut object = Map::new();
    object.insert("hooks".into(), hooks.clone());
    project_file(root, path.to_owned(), order, ConfigFile::Loaded(object))
}

#[test]
fn digests_match_the_node_golden() {
    let cases: Vec<Value> = serde_json::from_str(FIXTURE).unwrap();
    let golden: Vec<Value> = serde_json::from_str(GOLDEN).unwrap();
    assert_eq!(cases.len(), golden.len());
    for (case, expected) in cases.iter().zip(&golden) {
        let root = case["workspacePath"].as_str().unwrap();
        let files: Vec<ProjectFile> = case["sources"]
            .as_array()
            .unwrap()
            .iter()
            .map(|source| {
                file_with(
                    root,
                    source["path"].as_str().unwrap(),
                    usize::try_from(source["discoveryOrder"].as_u64().unwrap()).unwrap(),
                    &source["hooks"],
                )
            })
            .collect();
        let user = match &case["userHooks"] {
            Value::Null => NodeHooks::default(),
            value => NodeHooks::from_json(value).0,
        };
        let mut warnings = Vec::new();
        let found = candidates(&files, &mut warnings);
        assert!(warnings.is_empty(), "{warnings:?}");
        let root_config = workspace_digest::runtime_root(
            std::iter::once(&user).chain(found.iter().map(|(_, hooks)| hooks)),
        );
        let bundle = workspace_digest::bundle(&sources(root, &found), &root_config).unwrap();
        let name = case["name"].as_str().unwrap();
        assert_eq!(bundle.bundle_digest, expected["bundleDigest"], "{name}");
        let actual: Vec<Value> = bundle
            .entries
            .iter()
            .map(|entry| {
                json!({
                    "reviewItemId": entry.review_item_id,
                    "sourceRelativePath": entry.source_relative_path,
                    "resolvedTimeoutMs": entry.resolved_timeout_ms,
                    "configuredEnabled": entry.configured_enabled,
                    "editable": entry.editable,
                    "hookDeclarationDigest": entry.digest,
                })
            })
            .collect();
        // 数字按值比较（Node 的 2500 与这里的 2500.0）。
        let normalize = |values: &[Value]| -> Vec<String> {
            values
                .iter()
                .map(rust_cli_base::js::json::stringify)
                .collect()
        };
        assert_eq!(
            normalize(&actual),
            normalize(expected["hooks"].as_array().unwrap()),
            "{name}"
        );
    }
}

fn record(identity: &str, digest: &str) -> TrustRecord {
    TrustRecord {
        workspace_identity: identity.to_owned(),
        hook_declaration_digest: digest.to_owned(),
        granted_at: "2026-10-17T08:00:00.000Z".to_owned(),
        last_used_at: None,
        bundle_digest_at_grant: None,
        event_at_grant: "PreToolUse".to_owned(),
        display_command_at_grant: "x".to_owned(),
        source_path_at_grant: "zcode.json".to_owned(),
        source_discovery_order_at_grant: None,
        matcher_at_grant: None,
        matcher_index_at_grant: None,
        hook_index_at_grant: None,
        app_version_at_grant: None,
    }
}

#[tokio::test]
async fn only_trusted_enabled_declarations_run_and_the_rest_are_pending() {
    let home = tempfile::tempdir().unwrap();
    let paths = StoragePaths::resolve(&Default::default(), home.path(), home.path());
    let root = "/Users/dev/repo";
    let hooks = json!({"enabled": true, "events": {
        "PreToolUse": [{"matcher": "Bash", "hooks": [
            {"type": "command", "command": "trusted.sh"},
            {"type": "command", "command": "pending.sh"}
        ]}],
        "Notification": [{"hooks": [{"type": "command", "command": "notification-only.sh"}]}]
    }});
    let files = vec![file_with(root, "/Users/dev/repo/zcode.json", 0, &hooks)];
    let mut warnings = Vec::new();
    let first = WorkspaceHooks::inspect(root, None, &files, &paths, &mut warnings).await;
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(first.admitted().is_empty());
    let trusted = first.entries()[0].digest.clone();
    first
        .store
        .grant(vec![
            record(root, &trusted),
            record("/Users/dev/other", &first.entries()[1].digest),
        ])
        .await
        .unwrap();
    let again = WorkspaceHooks::inspect(root, None, &files, &paths, &mut Vec::new()).await;
    let admitted = again.admitted();
    assert_eq!(admitted.len(), 1);
    let (source, settings) = &admitted[0];
    assert_eq!(*source, HookSource::Project);
    let (event, matchers) = &settings.hooks[0];
    assert_eq!(*event, HookEventName::PreToolUse);
    assert_eq!(matchers[0].matcher.as_deref(), Some("Bash"));
    assert_eq!(matchers[0].hooks.len(), 1);
    assert!(matches!(
        &matchers[0].hooks[0],
        HookCommand::Command(command) if command.command == "trusted.sh" && command.timeout == Some(60.0)
    ));
    let pending = again.pending().unwrap();
    assert_eq!(pending.digests, [again.entries()[1].digest.clone()]);
    let bundle = again.bundle.as_ref().unwrap().bundle_digest.clone();
    assert_eq!(
        pending.notice(),
        format!(
            "Workspace Hooks skipped: workspace_hooks_pending_trust\nworkspace: /Users/dev/repo\n\
             bundle: {bundle}\npending digest: {}\n\
             Review with: zcode hooks trust review --workspace \"/Users/dev/repo\"\n",
            pending.digests[0]
        )
    );
}

#[tokio::test]
async fn the_user_storage_dir_moves_the_trust_store() {
    let home = tempfile::tempdir().unwrap();
    let paths = StoragePaths::resolve(&Default::default(), home.path(), home.path());
    let user = json!({"storage": {"dir": "elsewhere"}});
    let hooks = WorkspaceHooks::inspect("/w", user.as_object(), &[], &paths, &mut Vec::new()).await;
    assert_eq!(
        hooks.store.path().as_path(),
        home.path()
            .join("elsewhere/security/workspace-hook-trust-v1.json")
    );
    assert!(hooks.bundle.is_none());
    assert!(hooks.pending().is_none());
}
