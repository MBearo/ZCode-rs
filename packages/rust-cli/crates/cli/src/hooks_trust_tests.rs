//! `zcode hooks trust`（config.md §4.3）：参数、状态、grant / revoke 与 Node 的文本。
#![allow(clippy::disallowed_methods, reason = "测试读回写出的信任记录")]
use rust_cli_kernel_api::env::EnvFacts;
use rust_cli_kernel_api::testing::MemoryEnvironment;

use super::*;

const ROOT: &str = "/Users/dev/repo";

fn args(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_owned).collect()
}

fn env(hooks: &str) -> MemoryEnvironment {
    let facts = EnvFacts {
        cwd: EnvPath::new(ROOT),
        platform: "linux".to_owned(),
        os_version: "Linux".to_owned(),
        shell: "bash".to_owned(),
        is_git_repo: false,
        additional_directories: Vec::new(),
        home: Some(EnvPath::new("/Users/dev")),
    };
    MemoryEnvironment::new(facts).with_file(
        "/Users/dev/repo/.zcode/config.json",
        &format!(r#"{{"hooks": {hooks}}}"#),
    )
}

const HOOKS: &str = r#"{"enabled": true, "events": {"PreToolUse": [{"matcher": "Bash", "hooks": [
    {"type": "process", "command": "node", "args": ["guard.js"]},
    {"type": "command", "command": "off.sh", "enabled": false}
]}]}}"#;

async fn run_with(env: &MemoryEnvironment, paths: &StoragePaths, line: &str) -> Output {
    let command = parse(&args(line)).unwrap().unwrap();
    let context = TrustContext {
        env,
        paths,
        cwd: ROOT,
        version: "0.1.0",
        now_ms: 1_760_688_000_000,
    };
    run(command, &context).await
}

#[test]
fn arguments_follow_the_node_command() {
    assert_eq!(parse(&args("login")), None);
    assert_eq!(
        parse(&args("hooks nope")),
        Some(Err(format!("Unknown hooks command: nope\n{USAGE}")))
    );
    assert_eq!(
        parse(&args("hooks trust bogus")),
        Some(Err(format!("Unknown hooks trust command: bogus\n{USAGE}")))
    );
    assert_eq!(parse(&args("hooks trust -h")), Some(Ok(TrustCommand::Help)));
    let Some(Ok(TrustCommand::Run(action, options))) = parse(&args(
        "hooks trust grant --hook-digest a --hook-digest b --json",
    )) else {
        panic!("grant expected");
    };
    assert_eq!(action, Action::Grant);
    assert_eq!(options.digests, ["a", "b"]);
    assert!(options.json);
    assert_eq!(
        target(
            &Options {
                workspace: Some("local:abc".into()),
                ..Options::default()
            },
            ROOT
        ),
        (ROOT.to_owned(), Some("local:abc".to_owned()))
    );
    assert_eq!(
        target(
            &Options {
                workspace: Some("app".into()),
                ..Options::default()
            },
            ROOT
        ),
        ("/Users/dev/repo/app".to_owned(), None)
    );
}

#[tokio::test]
async fn status_grant_and_revoke_print_node_text() {
    let home = tempfile::tempdir().unwrap();
    let paths = StoragePaths::resolve(&Default::default(), home.path(), home.path());
    let env = env(HOOKS);
    let review = run_with(&env, &paths, "hooks trust review").await;
    assert_eq!(review.code, 0);
    let lines: Vec<&str> = review.stdout.lines().collect();
    assert_eq!(lines[0], "Workspace Hook Trust (review)");
    assert_eq!(lines[1], "workspace: /Users/dev/repo");
    assert_eq!(lines[4], "state: workspace_hooks_pending_trust");
    assert_eq!(lines[5], "1. [pending_trust] [enabled] PreToolUse / Bash");
    assert_eq!(lines[6], "   node guard.js");
    assert_eq!(lines[7], "   source: .zcode/config.json");
    assert_eq!(lines[9], "2. [pending_trust] [disabled] PreToolUse / Bash");
    assert_eq!(lines[13], "Pretrust exact declarations with:");
    let bundle = lines[3].trim_start_matches("bundle: ").to_owned();
    // --all-current 需要 bundle digest。
    let missing = run_with(&env, &paths, "hooks trust grant --all-current").await;
    assert_eq!(missing.stderr, "Error: workspace_hooks_bundle_changed\n");
    let both = run_with(
        &env,
        &paths,
        "hooks trust grant --all-current --hook-digest x",
    )
    .await;
    assert_eq!(
        both.stderr,
        "Error: Specify exactly one of --all-current or --hook-digest.\n"
    );
    let granted = run_with(
        &env,
        &paths,
        &format!("hooks trust grant --all-current --bundle-digest {bundle} --json"),
    )
    .await;
    assert_eq!(granted.code, 0, "{granted:?}");
    let status: serde_json::Value = serde_json::from_str(&granted.stdout).unwrap();
    assert_eq!(status["reasonCode"], "workspace_hooks_trusted_persistent");
    assert_eq!(status["items"][0]["trustState"], "trusted_persistent");
    assert_eq!(status["items"][1]["trustState"], "pending_trust");
    let keys: Vec<&str> = status
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "workspacePath",
            "workspaceIdentity",
            "bundleDigest",
            "reasonCode",
            "items"
        ]
    );
    // 记录写成 Node 的格式（Node CLI 与 App 能读）。
    let store = tokio::fs::read_to_string(paths.hook_trust_store(None).as_path())
        .await
        .unwrap();
    let saved: serde_json::Value = serde_json::from_str(&store).unwrap();
    let record = &saved["records"][0];
    assert_eq!(record["displayCommandAtGrant"], "node guard.js");
    assert_eq!(record["matcherAtGrant"], "Bash");
    assert_eq!(record["grantedAt"], "2025-10-17T08:00:00.000Z");
    assert_eq!(record["appVersionAtGrant"], "0.1.0");
    let mismatch = run_with(
        &env,
        &paths,
        "hooks trust grant --hook-digest deadbeef --json",
    )
    .await;
    assert_eq!(
        mismatch.stdout,
        "{\n  \"accepted\": false,\n  \"reasonCode\": \"workspace_hooks_snapshot_mismatch\"\n}\n"
    );
    let revoked = run_with(&env, &paths, "hooks trust revoke --all").await;
    assert!(
        revoked
            .stdout
            .contains("state: workspace_hooks_pending_trust")
    );
}

#[tokio::test]
async fn no_declarations_are_not_applicable() {
    let home = tempfile::tempdir().unwrap();
    let paths = StoragePaths::resolve(&Default::default(), home.path(), home.path());
    let env = env("{}");
    let status = run_with(&env, &paths, "hooks trust").await;
    assert!(status.stdout.contains(
        "state: workspace_hooks_not_applicable\nNo Workspace Hook declarations found.\n"
    ));
    let grant = run_with(&env, &paths, "hooks trust grant --hook-digest x").await;
    assert_eq!(
        grant.stderr,
        "Error: No Workspace Hook declarations were discovered.\n"
    );
}
