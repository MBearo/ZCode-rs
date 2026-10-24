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
    // 工作线 F80：复合命令里一段被拒绝时文字是整条命令（以前是那一段）。
    assert_eq!(
        verdict(PermissionMode::Default, &deny, "ls && rm -rf b"),
        "deny: Permission to use Bash with command ls && rm -rf b has been denied."
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

/// 工作线 F80（§4.5.5）：路径的 deny 规则拒绝时文字也是整条命令（以前是 `cat from '/etc/hosts' was blocked by a deny
/// rule.`）。
#[test]
fn path_deny_rules_block_shell_access() {
    let deny = [(RuleBehavior::Deny, "Read(//etc/**)")];
    assert_eq!(
        verdict(PermissionMode::BypassPermissions, &deny, "cat /etc/hosts"),
        "deny: Permission to use Bash with command cat /etc/hosts has been denied."
    );
}

/// 整条命令的建议规则（§4.10）；期望值是同一组命令在对照实现上的实测结果。
fn suggested(command: &str) -> Vec<String> {
    run(PermissionMode::Default, &[], |inputs| {
        match check(command, inputs) {
            Check::Ask { suggestions, .. } | Check::Passthrough { suggestions, .. } => suggestions
                .into_iter()
                .map(|rule| rule.content.unwrap_or_default())
                .collect(),
            _ => Vec::new(),
        }
    })
}

#[test]
fn compound_commands_combine_the_suggestions() {
    assert_eq!(
        suggested("mytool a && othertool b c"),
        ["mytool a *", "othertool b *"]
    );
    assert_eq!(
        suggested("mytool a || othertool b"),
        ["mytool a *", "othertool b *"]
    );
    assert_eq!(suggested("mytool a; mytool a"), ["mytool a *"]);
    assert_eq!(suggested("mytool a && mytool a"), ["mytool a *"]);
    assert_eq!(
        suggested("m1 x; m2 x; m3 x; m4 x; m5 x; m6 x"),
        ["m1 x *", "m2 x *", "m3 x *", "m4 x *", "m5 x *"]
    );
    assert_eq!(
        suggested("mytool a\nmytool b"),
        ["mytool a *", "mytool b *"]
    );
    assert_eq!(suggested("cd sub && mytool x"), ["mytool x *"]);
    // 管道按段接起来（对照实现每段一个 addRules，D17）。
    assert_eq!(suggested("mytool a | grep x"), ["mytool a *"]);
    assert_eq!(
        suggested("mytool a | othertool b"),
        ["mytool a *", "othertool b *"]
    );
}

#[test]
fn single_commands_use_the_subcommand_text() {
    assert_eq!(
        suggested("mytool build \\\n  --release"),
        ["mytool build *"]
    );
    assert_eq!(
        suggested("mytool --flag \\\n  --other"),
        ["mytool --flag --other"]
    );
    assert_eq!(suggested("mytool <<<hello"), ["mytool *"]);
    assert_eq!(suggested("mytool <<'EOF'\nx\nEOF"), ["mytool"]);
    assert_eq!(suggested("mytool sub \"unterminated"), ["mytool sub *"]);
    assert_eq!(suggested("mytool a 2>&1"), ["mytool a *"]);
    assert_eq!(suggested("mytool a > /dev/null"), ["mytool a *"]);
    assert_eq!(suggested("mytool '$X'"), ["mytool '$X'"]);
    assert_eq!(suggested("mytool \"a$\""), ["mytool \"a$\""]);
    assert_eq!(suggested("mytool $((1+2))"), ["mytool $((1+2))"]);
}

#[test]
fn some_commands_get_no_suggestion() {
    for command in [
        "mytool run <<EOF\nhello\nEOF",
        "mytool <<-EOF\n\tx\nEOF",
        "mytool $X",
        "mytool \"$X\"",
        "mytool `echo hi`",
        "mytool ${X}",
        "mytool $(echo hi)",
        "mytool a=$X",
        "X=$Y mytool sub",
        "mytool a && $X b",
        "(mytool a)",
        "{ mytool a; }",
        "for x in a; do mytool $x; done",
        "case x in a) mytool a;; esac",
        "f() { mytool a; }",
    ] {
        assert!(suggested(command).is_empty(), "{command}");
    }
}
