//! match query、matcher、去重与 `if` 条件（spec §3.3）。

use rust_cli_kernel_api::hooks::HookEventName as E;
use serde_json::json;

use super::*;
use crate::config::{CommandHook, HttpHook, PromptHook};

fn cmd(text: &str, condition: Option<&str>) -> HookCommand {
    HookCommand::Command(CommandHook {
        command: text.to_owned(),
        args: None,
        shell: HookShell::Bash,
        timeout: None,
        condition: condition.map(str::to_owned),
        background: false,
        plugin_root: None,
        deny_on_error: false,
    })
}

fn prompt(text: &str) -> HookCommand {
    HookCommand::Prompt(PromptHook {
        prompt: text.to_owned(),
        model: None,
        timeout: None,
        condition: None,
        continue_on_block: false,
    })
}

fn http(url: &str) -> HookCommand {
    HookCommand::Http(HttpHook {
        url: url.to_owned(),
        headers: Vec::new(),
        allowed_env_vars: Vec::new(),
        timeout: None,
        condition: None,
    })
}

fn matcher(pattern: Option<&str>, hooks: Vec<HookCommand>) -> HookMatcher {
    HookMatcher {
        matcher: pattern.map(str::to_owned),
        hooks,
    }
}

#[test]
fn match_queries_per_event() {
    let payload = json!({
        "tool_name": "Bash", "source": "startup", "trigger": "auto", "reason": "clear",
        "notification_type": "idle_prompt", "agent_type": "Explore", "file_path": "/w/src/a.rs",
        "error": "rate_limit", "load_reason": "include", "mcp_server_name": "srv"
    });
    for (event, expected) in [
        (E::PreToolUse, Some("Bash")),
        (E::PermissionDenied, Some("Bash")),
        (E::SessionStart, Some("startup")),
        (E::PreCompact, Some("auto")),
        (E::SessionEnd, Some("clear")),
        (E::Notification, Some("idle_prompt")),
        (E::SubagentStop, Some("Explore")),
        (E::FileChanged, Some("a.rs")),
        (E::StopFailure, Some("rate_limit")),
        (E::InstructionsLoaded, Some("include")),
        (E::Elicitation, Some("srv")),
        (E::Stop, None),
        (E::UserPromptSubmit, None),
        (E::TaskCreated, None),
    ] {
        assert_eq!(
            match_query(event, &payload).as_deref(),
            expected,
            "{event:?}"
        );
    }
}

#[test]
fn matcher_forms() {
    assert!(matches(E::PreToolUse, None, "Bash"));
    assert!(matches(E::PreToolUse, Some(""), "Bash"));
    assert!(matches(E::PreToolUse, Some("*"), "Bash"));
    assert!(matches(E::PreToolUse, Some("Bash"), "Bash"));
    assert!(!matches(E::PreToolUse, Some("Bash"), "BashOutput"));
    assert!(matches(E::PreToolUse, Some("Write|Edit"), "Edit"));
    // 工具类事件的列表允许逗号与空格。
    assert!(matches(E::PreToolUse, Some("Write, Edit"), "Edit"));
    // 其他事件不允许：按正则 `Write, Edit` 测试。
    assert!(!matches(E::Stop, Some("Write, Edit"), "Edit"));
    // 正则（JS 语义）。
    assert!(matches(E::PreToolUse, Some("^mcp__.*"), "mcp__srv__tool"));
    assert!(matches(E::PreToolUse, Some("Bash.*"), "BashOutput"));
    assert!(!matches(E::PreToolUse, Some("(unclosed"), "Bash"));
    // 旧名：列表与正则都认。
    assert!(matches(E::PreToolUse, Some("Task"), "Agent"));
    assert!(matches(E::PreToolUse, Some("^Task$"), "Agent"));
    assert!(matches(E::PreToolUse, Some("KillShell|Read"), "TaskStop"));
}

#[test]
fn select_filters_dedups_by_type_and_keeps_first_position() {
    let matchers = vec![
        matcher(
            Some("Bash"),
            vec![http("https://a.example.com"), cmd("one", None), prompt("p")],
        ),
        matcher(Some("Read"), vec![cmd("skipped", None)]),
        matcher(None, vec![cmd("two", None), cmd("one", None), prompt("p")]),
    ];
    let hooks = select(E::PreToolUse, &matchers, &json!({"tool_name": "Bash"}));
    let shown: Vec<String> = hooks.iter().map(HookCommand::display).collect();
    // command 组在前（one 保留第一次的位置），再 prompt，再 http。
    assert_eq!(shown, ["one", "two", "p", "https://a.example.com"]);
}

#[test]
fn if_conditions_and_session_start_http() {
    let matchers = vec![matcher(
        None,
        vec![
            cmd("git-only", Some("Bash(git commit:*)")),
            cmd("any-bash", Some("Bash")),
            cmd("glob", Some("Bash(npm * test)")),
            cmd("read", Some("Read(./a)")),
            http("https://a.example.com"),
        ],
    )];
    let shown = |payload| {
        select(E::PreToolUse, &matchers, &payload)
            .iter()
            .map(HookCommand::display)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        shown(json!({"tool_name": "Bash", "tool_input": {"command": "git commit -m x"}})),
        ["git-only", "any-bash", "https://a.example.com"]
    );
    assert_eq!(
        shown(json!({"tool_name": "Bash", "tool_input": {"command": "npm run test"}})),
        ["any-bash", "glob", "https://a.example.com"]
    );
    // 非工具事件上带 `if` 的 hook 跳过；SessionStart 不跑 http hook。
    let start = select(E::SessionStart, &matchers, &json!({"source": "startup"}));
    assert!(start.is_empty());
}

/// `if` 条件按权限规则的语法求值（hooks.md §3.3，kernel-api `permission::syntax`，F125 的 T4）。以前 hooks 自己写了一份
/// 简化的匹配：下面标“修前”的几行以前不匹配（`Bash(git *)` 不认 `git`、不并空白、不认转义的括号与星号、前缀不认
/// `xargs`），与判决链的同一条规则结论不同。
#[test]
fn if_conditions_use_the_permission_rule_syntax() {
    let hit = |condition: &str, command: &str| {
        condition_matches(condition, "Bash", &json!({ "command": command }))
    };
    // 修前不匹配：单个 ` *` 结尾时尾部可以省略。
    assert!(hit("Bash(git *)", "git"));
    assert!(hit("Bash(git *)", "git log -1"));
    // 修前不匹配：连续的空格、制表符并成一个空格。
    assert!(hit("Bash(git *)", "git\tlog"));
    assert!(hit("Bash(git status:*)", "git  status --short"));
    // 修前不匹配：content 里转义的括号。
    assert!(hit(r"Bash(echo \(hi\))", "echo (hi)"));
    // 修前不匹配：`\*` 是字面的星号。
    assert!(hit(r"Bash(echo \*)", "echo *"));
    assert!(!hit(r"Bash(echo \*)", "echo hi"));
    // 修前不匹配：前缀规则也认 `xargs <前缀>`。
    assert!(hit("Bash(rm:*)", "xargs rm -f x"));
    // 修前修后相同。
    assert!(hit("Bash(git:*)", "git"));
    assert!(!hit("Bash(git:*)", "gitk"));
    assert!(hit("Bash(npm run *)", "npm run build"));
    assert!(!hit("Bash(npm test)", "npm test -- x"));
    assert!(hit("Bash", "anything"));
    assert!(hit("Bash(*)", "anything"));
    assert!(hit("Bash()", "anything"));
    assert!(!hit("Read", "anything"));
    // 旧工具名与规则同一张表。
    assert!(condition_matches(
        "Task",
        "Agent",
        &json!({ "prompt": "x" })
    ));
    // 其他工具带内容的条件仍不匹配（D5）。
    assert!(!condition_matches(
        "Read(./a)",
        "Read",
        &json!({ "file_path": "./a" })
    ));
}
