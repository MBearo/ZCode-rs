//! `!` 内联命令（ext.md §2.5）。

use std::sync::Arc;

use rust_cli_kernel_api::permission::{
    PermissionMode, PermissionRules, RuleBehavior, RuleSource, SourcedRule,
};
use rust_cli_kernel_api::testing::{MemoryEnvironment, SequentialIds};

use super::*;
use crate::test_support::{EchoBash, facts, tool_context};

fn commands(text: &str) -> Vec<(String, String)> {
    find(text)
        .into_iter()
        .map(|inline| (inline.raw, inline.command))
        .collect()
}

#[test]
fn inline_and_block_forms() {
    assert_eq!(
        commands("Inline: !`echo inline-ok`\n"),
        [("!`echo inline-ok`".to_owned(), "echo inline-ok".to_owned())]
    );
    assert_eq!(
        commands("```!\nls -la\r\n```\nand !`pwd` too"),
        [
            ("```!\nls -la\r\n```".to_owned(), "ls -la".to_owned()),
            ("!`pwd`".to_owned(), "pwd".to_owned())
        ]
    );
    // 前面不是空白或行首的不算；普通行内代码里的 `!` 不算；空命令不算。
    assert!(commands("x!`no` and `a !`b` c` and !` `").is_empty());
}

fn runner(bash: Arc<EchoBash>, rules: &[(RuleBehavior, &str)]) -> Runner {
    let gate = PolicyGate::new(PermissionRules {
        version: 1,
        rules: rules
            .iter()
            .map(|(behavior, text)| SourcedRule {
                source: RuleSource::CliArg,
                behavior: *behavior,
                rule: rust_cli_policy::rules::parse_rule(text),
            })
            .collect(),
        ..PermissionRules::default()
    });
    Runner {
        bash,
        gate: Arc::new(gate),
        ids: Arc::new(SequentialIds::default()),
    }
}

#[tokio::test]
async fn allowed_tools_admit_commands_in_dont_ask_mode() {
    let bash = Arc::new(EchoBash::default());
    let context = tool_context(
        Arc::new(MemoryEnvironment::new(facts())),
        PermissionMode::DontAsk,
    );
    let expanded = runner(bash.clone(), &[])
        .expand(
            "A: !`echo one`\nB: !`echo two`\n".to_owned(),
            &["Bash(echo:*)".to_owned()],
            &context,
        )
        .await
        .unwrap();
    assert_eq!(expanded, "A: out:echo one\nB: out:echo two\n");
    // 每条命令一个新的 operationId，不复用 Skill 调用自己的。
    let ops: Vec<String> = bash
        .commands
        .lock()
        .unwrap()
        .iter()
        .map(|(_, op)| op.clone())
        .collect();
    assert_eq!(ops.len(), 2);
    assert!(ops.iter().all(|op| op != "op_call") && ops[0] != ops[1]);
}

#[tokio::test]
async fn unpermitted_and_failing_commands_fail_the_skill() {
    let context = tool_context(
        Arc::new(MemoryEnvironment::new(facts())),
        PermissionMode::DontAsk,
    );
    let bash = Arc::new(EchoBash::default());
    let denied = runner(bash.clone(), &[])
        .expand(
            "!`rm -rf x`".to_owned(),
            &["Bash(echo:*)".to_owned()],
            &context,
        )
        .await
        .unwrap_err();
    assert!(
        denied.starts_with("Shell command permission check failed for pattern \"!`rm -rf x`\": "),
        "{denied}"
    );
    assert!(bash.commands.lock().unwrap().is_empty());
    // deny 规则优先于 skill 的 allowed-tools。
    let blocked = runner(bash.clone(), &[(RuleBehavior::Deny, "Bash(echo:*)")])
        .expand(
            "!`echo hi`".to_owned(),
            &["Bash(echo:*)".to_owned()],
            &context,
        )
        .await
        .unwrap_err();
    assert!(
        blocked.starts_with("Shell command permission check failed"),
        "{blocked}"
    );
    let failed = runner(bash, &[])
        .expand(
            "!`echo fail`".to_owned(),
            &["Bash(echo:*)".to_owned()],
            &context,
        )
        .await
        .unwrap_err();
    assert_eq!(
        failed,
        "Shell command failed for pattern \"!`echo fail`\": out:echo fail"
    );
}
