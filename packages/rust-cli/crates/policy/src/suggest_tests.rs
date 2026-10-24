use super::*;

fn content(command: &str) -> String {
    suggestion(command).content.unwrap_or_default()
}

/// 期望值是同一组命令在对照实现上的实测结果（docs/specs/rust-cli/shell-permissions.md §4.10）。
#[test]
fn subcommands_get_a_two_word_prefix() {
    assert_eq!(content("mytool sub --flag value"), "mytool sub *");
    assert_eq!(content("docker compose up -d"), "docker compose *");
    assert_eq!(content("kubectl get pods -n prod"), "kubectl get *");
    assert_eq!(content("git push origin main"), "git push *");
    assert_eq!(content("npm run build -- --watch"), "npm run *");
    assert_eq!(
        content("./scripts/run.sh deploy now"),
        "./scripts/run.sh deploy *"
    );
    assert_eq!(content("Mytool sub"), "Mytool sub *");
    assert_eq!(
        content("/usr/local/bin/mytool sub x"),
        "/usr/local/bin/mytool sub *"
    );
    assert_eq!(content("mytool sub-cmd x"), "mytool sub-cmd *");
    assert_eq!(content("mytool x1 y"), "mytool x1 *");
    assert_eq!(content("  mytool   sub   x  "), "mytool sub *");
    assert_eq!(content("mytool\tsub x"), "mytool sub *");
}

#[test]
fn other_second_words_get_the_exact_command() {
    for command in [
        "mytool --flag",
        "mytool 755 file",
        "mytool Sub",
        "git -C /repo status",
        "mytool sub_cmd x",
        "mytool \"a b\" sub",
        "mytool 'sub' x",
        "mytool *.txt",
        "mytool ~/x",
        "mytool -- sub",
        "mytool @x sub",
        "mytool",
    ] {
        assert_eq!(content(command), command, "{command}");
    }
}

#[test]
fn only_safe_variables_are_skipped() {
    assert_eq!(
        content("NODE_ENV=production mytool build"),
        "mytool build *"
    );
    assert_eq!(content("FOO=1 mytool build"), "FOO=1 mytool build");
}

#[test]
fn wrappers_get_the_exact_command() {
    for command in [
        "timeout 10 mytool run",
        "sudo mytool install x",
        "bash -c \"mytool x\"",
        "env mytool x",
        "/usr/bin/nice mytool x",
    ] {
        assert_eq!(content(command), command, "{command}");
    }
}

#[test]
fn here_strings_and_heredocs_use_the_part_before_them() {
    assert_eq!(content("mytool <<<hello"), "mytool *");
    assert_eq!(content("mytool run <<EOF"), "mytool run *");
    assert_eq!(content("python3 -c x <<EOF"), "python3 -c *");
    assert_eq!(content("sudo mytool <<EOF"), "sudo mytool <<EOF");
    assert_eq!(content("<<EOF"), "<<EOF");
}

#[test]
fn several_lines_use_the_first_line() {
    assert_eq!(content("mytool first\nsecond line"), "mytool first *");
    assert_eq!(content("mytool --flag\nmore"), "mytool --flag *");
}

#[test]
fn word_shapes() {
    assert!(is_subcommand_word("sub-cmd"));
    assert!(is_subcommand_word("x1"));
    assert!(!is_subcommand_word("sub-"));
    assert!(!is_subcommand_word("1x"));
    assert!(!is_subcommand_word("Sub"));
    assert!(is_assignment("A_1=x"));
    assert!(!is_assignment("1A=x"));
    assert!(!is_assignment("a-b=x"));
    assert!(starts_with_wrapper("FOO=1 timeout 10 x"));
    assert!(!starts_with_wrapper("NODE_ENV=x npm run"));
    assert_eq!(
        first_word_prefix("git -C /repo status").as_deref(),
        Some("git")
    );
    assert_eq!(first_word_prefix("timeout 10 x"), None);
    assert_eq!(first_word_prefix("FOO=1 mytool"), None);
    assert_eq!(first_word_prefix("./run.sh x"), None);
}
