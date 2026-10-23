//! Node 配置的 `onError: "deny"`（hooks.md §4.1 末段）：PreToolUse 的 hook 出错时拦下工具，调用方取消不算出错；
//! 其他事件与没写这个字段的 hook 照旧出错放行。

use std::sync::Arc;

use rust_cli_kernel_api::hooks::{HookEventName, HookOutcome};
use rust_cli_kernel_api::permission::PermissionDecision;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::config::{HookSource, HooksConfig};
use crate::node_config::NodeHooks;
use crate::test_support::{Script, ScriptedEnv, event, ports, script};
use crate::{HookEngine, HookRunner};

const BLOCKED: &str = "PreToolUse:Bash hook error: [guard.sh]: PreToolUse hook failed and its onError is \"deny\", so the tool call is blocked: ";

fn node_engine(env: Arc<ScriptedEnv>, hook: Value, event_name: &str) -> HookEngine {
    let (hooks, warnings) = NodeHooks::from_json(&json!({
        "enabled": true,
        "events": {event_name: [{"matcher": "Bash", "hooks": [hook]}]}
    }));
    assert!(warnings.is_empty(), "{warnings:?}");
    let (settings, warnings) = hooks.user_settings();
    assert!(warnings.is_empty(), "{warnings:?}");
    HookEngine::with_config(
        ports(env),
        HooksConfig {
            version: 1,
            sources: vec![(HookSource::User, settings)],
            ..HooksConfig::default()
        },
    )
}

async fn pre_tool_use(env: Arc<ScriptedEnv>, hook: Value) -> HookOutcome {
    let engine = node_engine(env, hook, "PreToolUse");
    let pre = event(
        HookEventName::PreToolUse,
        json!({"tool_name": "Bash", "tool_input": {"command": "ls"}, "tool_use_id": "toolu_1"}),
    );
    engine.run(pre, CancellationToken::new()).await
}

fn deny(detail: &str) -> Option<PermissionDecision> {
    Some(PermissionDecision::Deny {
        message: format!("{BLOCKED}{detail}"),
    })
}

fn guard(on_error: Option<&str>) -> Value {
    let mut hook = json!({"type": "command", "command": "guard.sh"});
    if let Some(value) = on_error {
        hook["onError"] = json!(value);
    }
    hook
}

#[tokio::test]
async fn a_failing_guard_with_on_error_deny_blocks_the_tool() {
    let env = Arc::new(ScriptedEnv::with(&[(
        "guard.sh",
        script("", "  boom \n", 1),
    )]));
    let outcome = pre_tool_use(env, guard(Some("deny"))).await;
    assert_eq!(outcome.decision, deny("boom"));
}

#[tokio::test]
async fn without_stderr_the_reason_names_the_failed_status() {
    let env = Arc::new(ScriptedEnv::with(&[("guard.sh", script("", "", 3))]));
    let outcome = pre_tool_use(env, guard(Some("deny"))).await;
    assert_eq!(
        outcome.decision,
        deny("Hook process exited with status failed")
    );
}

#[tokio::test]
async fn a_timeout_blocks_the_tool() {
    let slow = Script {
        delay_ms: 5_000,
        ..Script::default()
    };
    let env = Arc::new(ScriptedEnv::with(&[("guard.sh", slow)]));
    let mut hook = guard(Some("deny"));
    hook["timeoutMs"] = json!(20);
    let outcome = pre_tool_use(env, hook).await;
    assert_eq!(outcome.decision, deny("Hook timed out after 20ms"));
}

#[tokio::test]
async fn a_hook_that_cannot_start_blocks_the_tool() {
    let env = Arc::new(ScriptedEnv::failing());
    let outcome = pre_tool_use(env, guard(Some("deny"))).await;
    let Some(PermissionDecision::Deny { message }) = outcome.decision else {
        panic!("not blocked: {outcome:?}");
    };
    assert!(
        message.starts_with(BLOCKED) && message.contains("spawn failed"),
        "{message}"
    );
}

#[tokio::test]
async fn failures_without_on_error_deny_still_let_the_tool_run() {
    for on_error in [None, Some("allow")] {
        let env = Arc::new(ScriptedEnv::with(&[("guard.sh", script("", "boom", 1))]));
        let outcome = pre_tool_use(env, guard(on_error)).await;
        assert_eq!(outcome.decision, None, "{on_error:?}");
    }
}

#[tokio::test]
async fn a_successful_or_blocking_guard_is_unchanged() {
    let env = Arc::new(ScriptedEnv::with(&[("guard.sh", script("", "", 0))]));
    assert_eq!(pre_tool_use(env, guard(Some("deny"))).await.decision, None);
    let env = Arc::new(ScriptedEnv::with(&[("guard.sh", script("", "no rm", 2))]));
    assert_eq!(
        pre_tool_use(env, guard(Some("deny"))).await.decision,
        Some(PermissionDecision::Deny {
            message: "PreToolUse:Bash hook error: [guard.sh]: no rm".to_owned()
        })
    );
}

#[tokio::test]
async fn cancelling_the_call_is_not_an_error() {
    let slow = Script {
        delay_ms: 5_000,
        ..Script::default()
    };
    let env = Arc::new(ScriptedEnv::with(&[("guard.sh", slow)]));
    let engine = node_engine(env, guard(Some("deny")), "PreToolUse");
    let pre = event(
        HookEventName::PreToolUse,
        json!({"tool_name": "Bash", "tool_input": {"command": "ls"}, "tool_use_id": "toolu_1"}),
    );
    let cancel = CancellationToken::new();
    let run = engine.run(pre, cancel.clone());
    let stop = async {
        tokio::task::yield_now().await;
        cancel.cancel();
    };
    let (outcome, ()) = tokio::join!(run, stop);
    assert_eq!(outcome.decision, None);
}

#[tokio::test]
async fn other_events_ignore_on_error() {
    let env = Arc::new(ScriptedEnv::with(&[("guard.sh", script("", "boom", 1))]));
    let engine = node_engine(env, guard(Some("deny")), "PostToolUse");
    let post = event(
        HookEventName::PostToolUse,
        json!({"tool_name": "Bash", "tool_input": {"command": "ls"}, "tool_response": {}, "tool_use_id": "toolu_1"}),
    );
    let outcome = engine.run(post, CancellationToken::new()).await;
    assert_eq!(outcome.decision, None);
    assert!(!outcome.prevent_continuation, "{outcome:?}");
}

#[test]
fn on_error_is_parsed_from_the_node_config() {
    let parse = |value: Value| {
        NodeHooks::from_json(&json!({
            "events": {"PreToolUse": [{"hooks": [{"type": "command", "command": "g", "onError": value}]}]}
        }))
    };
    let (hooks, warnings) = parse(json!("deny"));
    assert!(warnings.is_empty());
    assert!(hooks.matchers(HookEventName::PreToolUse)[0].hooks[0].deny_on_error);
    let (hooks, _) = parse(json!("allow"));
    assert!(!hooks.matchers(HookEventName::PreToolUse)[0].hooks[0].deny_on_error);
    let (hooks, warnings) = parse(json!("block"));
    assert!(hooks.matchers(HookEventName::PreToolUse).is_empty());
    assert_eq!(
        warnings,
        ["hooks.events.PreToolUse[0].hooks[0]: `onError` must be \"allow\" or \"deny\""]
    );
}
