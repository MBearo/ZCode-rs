//! Bash 拦前台长 `sleep` 的判定（tools-more.md §19.9；MO1 的 `sleep 30 && echo done`）。

use super::*;

#[test]
fn long_leading_sleeps_are_blocked() {
    assert_eq!(
        blocked("sleep 30 && echo done").as_deref(),
        Some("sleep 30 followed by: echo done")
    );
    assert_eq!(blocked("sleep 25").as_deref(), Some("standalone sleep 25"));
    assert_eq!(
        blocked("  sleep 30.  ").as_deref(),
        Some("standalone sleep 30")
    );
    assert_eq!(
        blocked("sleep 60.5; ls; pwd").as_deref(),
        Some("sleep 60.5 followed by: ls pwd")
    );
}

#[test]
fn short_or_later_sleeps_pass() {
    assert_eq!(blocked("sleep 2"), None);
    assert_eq!(blocked("sleep 24.9 && echo x"), None);
    assert_eq!(blocked("echo x && sleep 30"), None);
    assert_eq!(blocked("sleep 30s"), None);
    assert_eq!(blocked("until test -f x; do sleep 2; done"), None);
    assert_eq!(blocked(""), None);
}

/// Monitor 开着时 Bash 的描述多一句（MO0 实测，`-p` 与交互会话都加在 “No `&` needed.” 之后），前台长 `sleep` 拦下。
#[tokio::test]
async fn the_bash_tool_follows_the_monitor_switch() {
    use rust_cli_kernel_api::permission::PermissionMode;
    use rust_cli_kernel_api::tool::{PromptContext, Tool, Validation};
    use serde_json::json;

    let prompt = PromptContext {
        tool_names: Vec::new(),
        mode: PermissionMode::Default,
    };
    let sentence =
        " Foreground `sleep` is blocked; use Monitor with an until-loop to wait on a condition.";
    let print = crate::bash::BashTool::new().with_monitor(true);
    assert!(print.description(&prompt).contains(&format!(
        "at that limit it is stopped and you are re-invoked. No `&` needed.{sentence}"
    )));
    let session = crate::bash::BashTool::new()
        .with_monitor(true)
        .with_background_deadline(false);
    assert!(session.description(&prompt).contains(&format!(
        "re-invokes you when it exits. No `&` needed.{sentence}"
    )));
    assert!(
        !crate::bash::BashTool::new()
            .description(&prompt)
            .contains("Foreground `sleep`")
    );
    let context = crate::test_more::Ctx::new(std::sync::Arc::new(
        rust_cli_kernel_api::testing::ScriptedInteractions::default(),
    ))
    .build();
    let blocked = print
        .validate_input(&json!({"command": "sleep 30 && echo done"}), &context)
        .await;
    assert_eq!(
        blocked,
        Validation::Invalid {
            message: "Blocked: sleep 30 followed by: echo done. To wait for a condition, use Monitor with an until-loop (e.g. `until <check>; do sleep 2; done`). To wait for a command you started, use run_in_background: true. Do not chain shorter sleeps to work around this block.".to_owned(),
            error_code: Some(10),
        }
    );
    let background = json!({"command": "sleep 30", "run_in_background": true});
    assert_eq!(
        print.validate_input(&background, &context).await,
        Validation::Ok
    );
    let off = crate::bash::BashTool::new()
        .validate_input(&json!({"command": "sleep 30"}), &context)
        .await;
    assert_eq!(off, Validation::Ok);
}
