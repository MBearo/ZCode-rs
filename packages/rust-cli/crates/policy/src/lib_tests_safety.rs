//! 询问的性质（shell-permissions.md §4.1）：敏感文件 yolo 放行、guarded 询问（工作线 F76）、hook 放行后仍询问；
//! 危险删除 guarded 与 build 询问、allow 规则盖不过；命令替换太复杂的询问 bypass 类模式放行。yolo 下除拒绝外一律
//! 放行（§4.6.2，工作线 F76，用户决定 2026-10-23“如果是 yolo 啥也不问”），plan 开着时照旧询问。

use super::*;

const SENSITIVE: [&str; 3] = ["/w/.bashrc", "/w/.git/config", "/w/.vscode/settings.json"];

async fn write(gate: &PolicyGate, mode: PermissionMode, path: &str) -> PermissionDecision {
    decide(
        gate,
        mode,
        "Write",
        json!({"file_path": path, "content": "x"}),
    )
    .await
}

async fn hook_check(
    gate: &PolicyGate,
    mode: PermissionMode,
    tool: &'static str,
    input: Value,
) -> Option<PermissionDecision> {
    gate.check_rules(
        &Named(tool, PermissionDecision::Passthrough),
        &input,
        &context(mode),
    )
    .await
}

#[tokio::test]
async fn yolo_writes_sensitive_files() {
    let gate = PolicyGate::default();
    for path in SENSITIVE {
        assert_eq!(
            write(&gate, PermissionMode::BypassPermissions, path).await,
            allow(),
            "{path}"
        );
    }
}

/// 工作线 F76（用户决定 2026-10-23）：guarded 下写敏感文件询问（App 对自主模式的说明是“在有风险时询问”）。
/// F68 按 bypass 放行过。
#[tokio::test]
async fn guarded_asks_before_writing_sensitive_files() {
    let gate = PolicyGate::new(rules(&[(RuleBehavior::Allow, "Write")]));
    for path in SENSITIVE {
        assert_eq!(
            ask_message(&write(&gate, PermissionMode::Guarded, path).await),
            format!("ZCode requested permissions to edit {path} which is a sensitive file."),
            "{path}"
        );
    }
    // 别的写入照旧放行（guarded 在危险命令以外同 yolo）。
    assert_eq!(
        write(&gate, PermissionMode::Guarded, "/w/src/a.rs").await,
        allow()
    );
}

#[tokio::test]
async fn other_modes_still_ask_for_sensitive_files_even_with_an_allow_rule() {
    let gate = PolicyGate::new(rules(&[(RuleBehavior::Allow, "Write")]));
    for mode in [PermissionMode::Default, PermissionMode::AcceptEdits] {
        assert_eq!(
            ask_message(&write(&gate, mode, "/w/.git/config").await),
            "ZCode requested permissions to edit /w/.git/config which is a sensitive file.",
            "{mode:?}"
        );
    }
}

#[tokio::test]
async fn yolo_keeps_deny_rules_on_sensitive_files_but_not_ask_rules() {
    let gate = PolicyGate::new(rules(&[
        (RuleBehavior::Deny, "Edit(.git/**)"),
        (RuleBehavior::Ask, "Edit(.bashrc)"),
    ]));
    let mode = PermissionMode::BypassPermissions;
    assert_eq!(
        write(&gate, mode, "/w/.git/config").await,
        deny("Permission to edit /w/.git/config has been denied.")
    );
    assert_eq!(write(&gate, mode, "/w/.bashrc").await, allow());
    // guarded：ask 规则排在敏感文件之前，询问的文字是 ask 规则的。
    assert_eq!(
        ask_message(&write(&gate, PermissionMode::Guarded, "/w/.bashrc").await),
        "ZCode requested permissions to write to /w/.bashrc, but you haven't granted it yet."
    );
}

#[tokio::test]
async fn a_hook_cannot_approve_a_sensitive_write_unless_the_session_is_yolo() {
    let gate = PolicyGate::default();
    let input = json!({"file_path": "/w/.bashrc", "content": "x"});
    for mode in [PermissionMode::Default, PermissionMode::Guarded] {
        let decision = hook_check(&gate, mode, "Write", input.clone()).await;
        assert!(
            matches!(decision, Some(PermissionDecision::Ask { .. })),
            "{mode:?} {decision:?}"
        );
    }
    assert_eq!(
        hook_check(&gate, PermissionMode::BypassPermissions, "Write", input).await,
        None
    );
}

// 测试的工作区根 /w 是根下的一级目录，所以按关键目录询问。
const REMOVE_ROOT: &str = "Dangerous rm operation detected: '/w/*'\n\nThis command would remove a critical system directory. This requires explicit approval and cannot be auto-allowed by permission rules.";
const REMOVE_USR: &str = "Dangerous rmdir operation detected: '/usr'\n\nThis command would remove a critical system directory. This requires explicit approval and cannot be auto-allowed by permission rules.";

#[tokio::test]
async fn allow_rules_cannot_approve_a_dangerous_removal() {
    let whole = PolicyGate::new(rules(&[(RuleBehavior::Allow, "Bash")]));
    assert_eq!(
        ask_message(&bash(&whole, PermissionMode::Default, "rm -rf *").await),
        REMOVE_ROOT
    );
    // guarded 的危险命令规则先问（Node 的文字）；它不认的删除按危险删除问。
    let gate = PolicyGate::default();
    assert_eq!(
        ask_message(&bash(&gate, PermissionMode::Guarded, "rm -rf *").await),
        texts::GUARDED_ASK
    );
    assert_eq!(
        ask_message(&bash(&gate, PermissionMode::Guarded, "rmdir /usr").await),
        REMOVE_USR
    );
    let decision = hook_check(
        &gate,
        PermissionMode::Default,
        "Bash",
        json!({"command": "rm -rf *"}),
    )
    .await;
    assert!(
        matches!(decision, Some(PermissionDecision::Ask { .. })),
        "{decision:?}"
    );
}

/// 工作线 F76（用户决定 2026-10-23“如果是 yolo 啥也不问”）：yolo 下危险删除、ask 规则、hook 放行后的检查都放行
/// （F68 起询问过，D21）；deny 规则照旧拒绝；plan 开着时照旧询问。
#[tokio::test]
async fn yolo_never_asks() {
    let mode = PermissionMode::BypassPermissions;
    let gate = PolicyGate::new(rules(&[
        (RuleBehavior::Ask, "Bash(git push:*)"),
        (RuleBehavior::Deny, "Bash(git reset:*)"),
    ]));
    for command in [
        "rm -rf *",
        "rmdir /usr",
        "rm -rf ~",
        "rm -rf $(echo gone)",
        "rmdir \"$DIR\"/",
        "git push origin main",
        "(rm -rf ~)",
        "echo a & echo b",
    ] {
        assert_eq!(bash(&gate, mode, command).await, allow(), "{command}");
        assert_eq!(
            hook_check(&gate, mode, "Bash", json!({"command": command})).await,
            None,
            "{command}"
        );
    }
    assert_eq!(
        bash(&gate, mode, "git reset --hard").await,
        deny(texts::bash_command_denied("git reset --hard"))
    );
    // 工具自身的询问也放行，拒绝照旧。
    let own_ask = Named(
        "Other",
        PermissionDecision::Ask {
            message: "why".to_owned(),
            suggestions: Vec::new(),
        },
    );
    assert_eq!(
        gate.decide(&own_ask, &json!({}), &context(mode)).await,
        allow()
    );
    // plan 开着：判决用的模式是 plan，危险删除照旧询问（plan 的询问交给分类器，不可用时拒绝）。
    use rust_cli_kernel_api::permission::ExecutionState;
    let planning = ExecutionState::from(mode).with_plan(true).effective();
    assert_eq!(
        ask_message(&bash(&PolicyGate::default(), planning, "rm -rf *").await),
        REMOVE_ROOT
    );
}

#[tokio::test]
async fn a_too_complex_command_still_runs_in_bypass() {
    let gate = PolicyGate::new(rules(&[(RuleBehavior::Allow, "Bash(echo:*)")]));
    assert_eq!(
        bash(&gate, PermissionMode::BypassPermissions, "echo $(mkdir x)").await,
        allow()
    );
    assert_eq!(
        ask_message(&bash(&gate, PermissionMode::Default, "echo $(mkdir x)").await),
        "A nested command in this command can't be checked before it runs"
    );
}

/// 工作线 F76：后台 `&` 的询问是安全检查：build 询问、allow 规则盖不过、hook 放行之后仍询问；yolo 放行。
/// 工作线 F80（用户决定 2026-10-24“同 Node”）：guarded 不问，hook 放行之后也不问（F76 询问过）。
#[tokio::test]
async fn the_background_question_is_a_safety_check_outside_guarded() {
    let gate = PolicyGate::new(rules(&[(RuleBehavior::Allow, "Bash(echo:*)")]));
    let background = "This command uses the `&` background operator, which defers execution past approval-time safety checks. Approve only if you trust it.";
    let input = json!({"command": "echo a & echo b"});
    assert_eq!(
        ask_message(&bash(&gate, PermissionMode::Default, "echo a & echo b").await),
        background
    );
    assert!(matches!(
        hook_check(&gate, PermissionMode::Default, "Bash", input.clone()).await,
        Some(PermissionDecision::Ask { .. })
    ));
    for mode in [PermissionMode::Guarded, PermissionMode::BypassPermissions] {
        assert_eq!(
            bash(&gate, mode, "echo a & echo b").await,
            allow(),
            "{mode:?}"
        );
        assert_eq!(
            hook_check(&gate, mode, "Bash", input.clone()).await,
            None,
            "{mode:?}"
        );
    }
    // 子 shell 的询问是其他：guarded 放行。
    assert_eq!(
        bash(&gate, PermissionMode::Guarded, "(echo a && echo b)").await,
        allow()
    );
}
