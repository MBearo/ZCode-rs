//! Bash 检查：只读放行、规则、路径、复合命令；询问文本逐字比较。

use rust_cli_kernel_api::permission::{PermissionMode, RuleBehavior};

use super::*;
use crate::test_support::{message, run};

fn verdict(mode: PermissionMode, entries: &[(RuleBehavior, &str)], command: &str) -> String {
    run(mode, entries, |inputs| message(&check(command, inputs)))
}

fn default(command: &str) -> String {
    verdict(PermissionMode::Default, &[], command)
}

#[test]
fn read_only_commands_are_allowed() {
    for command in [
        "echo hello",
        "ls",
        "cat a.txt | head -1",
        "git status",
        "pwd",
        "grep -n x a.txt",
    ] {
        assert_eq!(default(command), "allow", "{command}");
    }
}

#[test]
fn writes_inside_the_working_directory_ask_with_the_fixed_text() {
    // touch asked.txt（default 模式）。
    assert_eq!(
        default("touch asked.txt"),
        "ask: touch in '/w/asked.txt' needs approval. The path is inside the working directories for this session ('/w'), and ZCode asks before a shell command creates, changes or removes files there."
    );
    assert_eq!(
        default("echo hi > out.txt"),
        "ask: Output redirection to '/w/out.txt' needs approval. The path is inside the working directories for this session ('/w'), and ZCode asks before a shell command creates, changes or removes files there."
    );
}

#[test]
fn accept_edits_allows_file_commands_inside_and_asks_outside() {
    let accept = |command| verdict(PermissionMode::AcceptEdits, &[], command);
    assert_eq!(accept("mkdir made && touch made/x.txt"), "allow");
    assert_eq!(
        accept("touch /tmp/zcode-outside.txt"),
        "ask: touch in '/tmp/zcode-outside.txt' needs approval. The path is outside the working directories for this session ('/w'). Allowing runs the command as written."
    );
    assert_eq!(
        accept("npm --version"),
        "passthrough: This command requires approval"
    );
}

#[test]
fn deny_rules_match_prefixes_after_safe_wrappers() {
    let deny = [(RuleBehavior::Deny, "Bash(rm:*)")];
    assert_eq!(
        verdict(PermissionMode::BypassPermissions, &deny, "rm -f a.txt"),
        "deny: Permission to use Bash with command rm -f a.txt has been denied."
    );
    assert_eq!(
        verdict(PermissionMode::Default, &deny, "timeout 5 rm x"),
        "deny: Permission to use Bash with command timeout 5 rm x has been denied."
    );
    assert_eq!(
        verdict(PermissionMode::Default, &deny, "ls && rm -rf b"),
        "deny: Permission to use Bash with command rm -rf b has been denied."
    );
    assert!(verdict(PermissionMode::Default, &deny, "rmdir x").starts_with("ask:"));
}

#[test]
fn allow_rules_need_every_subcommand() {
    let allow = [(RuleBehavior::Allow, "Bash(npm run test:*)")];
    assert_eq!(
        verdict(PermissionMode::Default, &allow, "npm run test -- --watch"),
        "allow"
    );
    assert_eq!(verdict(PermissionMode::Default, &allow, "npm run test && npm --version"), "ask: This Bash command contains multiple operations. The following part requires approval: npm --version".replace("ask:", "passthrough:"));
    assert!(verdict(PermissionMode::Default, &allow, "npm run testing").starts_with("passthrough"));
}

#[test]
fn ask_rules_name_their_source() {
    let ask = [(RuleBehavior::Ask, "Bash(git push:*)")];
    assert_eq!(
        verdict(PermissionMode::Default, &ask, "git push origin main"),
        "ask: Permission rule 'Bash(git push:*)' from CLI argument requires approval for this Bash command"
    );
}

#[test]
fn compound_commands_with_cd_and_writes_ask() {
    assert_eq!(
        default("mkdir -p sub && cd sub && pwd"),
        format!("ask: {}", crate::texts::CD_WITH_WRITE)
    );
}

#[test]
fn compound_asks_list_the_parts_that_need_approval() {
    assert_eq!(
        default("ls && npm --version && touch b.txt"),
        "ask: This Bash command contains multiple operations. The following parts require approval: npm --version, touch b.txt"
    );
}

#[test]
fn reads_outside_the_working_directories_ask() {
    assert_eq!(
        default("cat /etc/hosts"),
        "ask: cat in '/etc/hosts' was blocked. For security, ZCode may only concatenate files from the allowed working directories for this session: '/w'."
    );
    // 命令输出目录不必询问。
    assert_eq!(
        default("cat /tmp/zcode-shell/sessions/s1/tasks/b1.output"),
        "allow"
    );
}

#[test]
fn path_deny_rules_block_shell_access() {
    let deny = [(RuleBehavior::Deny, "Read(//etc/**)")];
    assert_eq!(
        verdict(PermissionMode::BypassPermissions, &deny, "cat /etc/hosts"),
        "deny: cat from '/etc/hosts' was blocked by a deny rule."
    );
}

#[test]
fn suggestions_use_the_command_prefix() {
    assert_eq!(
        suggestion("npm install left-pad").content.as_deref(),
        Some("npm install:*")
    );
    assert_eq!(suggestion("make -j8").content.as_deref(), Some("make:*"));
    assert_eq!(
        suggestion("python script.py").content.as_deref(),
        Some("python script.py")
    );
}
