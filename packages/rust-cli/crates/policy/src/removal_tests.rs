//! 危险删除（shell-permissions.md §4.5.3）：删除工作目录、它的上级、关键目录，以及静态看不出目标的删除都要询问，
//! 询问的性质是 bypass 也问、allow 规则盖不过。文字逐字比较。

use rust_cli_kernel_api::env_files::PathStyle;
use rust_cli_kernel_api::permission::{PermissionMode, RuleBehavior};

use crate::bash::check;
use crate::check::{AskKind, Check};
use crate::paths::PathContext;
use crate::test_support::{message, run_in};

const CWD: &str = "/srv/repos/proj";
const TAIL: &str =
    "This requires explicit approval and cannot be auto-allowed by permission rules.";

fn paths() -> PathContext {
    PathContext {
        cwd: CWD.to_owned(),
        roots: vec![CWD.to_owned()],
        home: Some("/home/dev".to_owned()),
        style: PathStyle::Posix,
        internal: Vec::new(),
        plans: None,
    }
}

fn outcome(mode: PermissionMode, entries: &[(RuleBehavior, &str)], command: &str) -> Check {
    run_in(&paths(), mode, entries, |inputs| check(command, inputs))
}

fn verdict(command: &str) -> String {
    message(&outcome(PermissionMode::BypassPermissions, &[], command))
}

fn literal(cmd: &str, target: &str, why: &str) -> String {
    format!("ask: Dangerous {cmd} operation detected: '{target}'\n\n{why} {TAIL}")
}

const WORKSPACE: &str = "This command would remove a workspace directory (the working directory, an additional working directory, or one of their parent directories).";
const CRITICAL: &str = "This command would remove a critical system directory.";
const UNRESOLVED: &str =
    "This command's removal target cannot be statically resolved to a directory.";

#[test]
fn removing_the_working_directory_or_its_parents_asks() {
    assert_eq!(
        verdict("rm -rf /srv/repos/proj"),
        literal("rm", CWD, WORKSPACE)
    );
    assert_eq!(verdict("rm -rf ."), literal("rm", CWD, WORKSPACE));
    assert_eq!(verdict("rm -rf .."), literal("rm", "/srv/repos", WORKSPACE));
    assert_eq!(
        verdict("rm -rf *"),
        literal("rm", "/srv/repos/proj/*", WORKSPACE)
    );
    assert_eq!(
        verdict("rm -r ./*"),
        literal("rm", "/srv/repos/proj/*", WORKSPACE)
    );
    // 大小写不同也算同一个目录。
    assert_eq!(
        verdict("rm -rf /SRV/repos"),
        literal("rm", "/SRV/repos", WORKSPACE)
    );
}

#[test]
fn removing_a_critical_directory_asks() {
    assert_eq!(verdict("rm -rf ../.."), literal("rm", "/srv", CRITICAL));
    assert_eq!(verdict("rm -rf ~"), literal("rm", "/home/dev", CRITICAL));
    assert_eq!(verdict("rm -rf ~/"), literal("rm", "/home/dev/", CRITICAL));
    assert_eq!(
        verdict("rm -rf $HOME"),
        literal("rm", "/home/dev", CRITICAL)
    );
    assert_eq!(
        verdict("rm -rf \"$HOME\""),
        literal("rm", "/home/dev", CRITICAL)
    );
    assert_eq!(
        verdict("rm -rf ~/*"),
        literal("rm", "/home/dev/*", CRITICAL)
    );
    assert_eq!(verdict("rmdir /usr"), literal("rmdir", "/usr", CRITICAL));
    assert_eq!(verdict("rm -rf /tmp/"), literal("rm", "/tmp/", CRITICAL));
    // macOS 上 /private/etc 就是 /etc。
    if cfg!(target_os = "macos") {
        assert_eq!(
            verdict("rm -rf /private/etc"),
            literal("rm", "/private/etc", CRITICAL)
        );
    }
}

#[test]
fn globs_that_cannot_be_resolved_ask() {
    assert_eq!(
        verdict("rm -rf sub/*/*"),
        literal(
            "rm",
            "/srv/repos/proj/sub/*/*",
            "This command's glob pattern traverses directories that cannot be statically enumerated."
        )
    );
    assert_eq!(
        verdict("rm -rf ../*"),
        literal("rm", "/srv/repos/*", UNRESOLVED)
    );
    assert_eq!(
        verdict("rm -rf ~other/x/*"),
        literal("rm", "/srv/repos/proj/~other/x/*", UNRESOLVED)
    );
    assert_eq!(
        verdict("cd sub && rm -rf *"),
        literal(
            "rm",
            "/srv/repos/proj/*",
            "This command changes directories before the removal, so the relative glob target cannot be statically resolved."
        )
    );
}

#[test]
fn ordinary_removals_are_not_dangerous() {
    for command in [
        "rm -rf build",
        "rm -rf sub/*",
        "rm -f /srv/repos/proj/a.txt",
        "rmdir empty",
        "rm -rf $DIR/build",
        "rm -- -rf",
    ] {
        let result = verdict(command);
        assert!(!result.contains("Dangerous"), "{command}: {result}");
    }
}

fn variable(cmd: &str, invocation: &str, target: &str, becomes: &str, advice: &str) -> String {
    format!(
        "ask: Dangerous {cmd} operation detected in `{invocation}`. The target '{target}' is a shell variable expansion: when $DIR is unset or empty it becomes {becomes}. {TAIL}\n\nThis check does not fire on a target that cannot expand to the filesystem root: {advice}."
    )
}

#[test]
fn a_variable_that_may_be_empty_in_front_of_a_slash_asks() {
    assert_eq!(
        verdict("rm -rf $DIR/"),
        variable(
            "rm",
            "rm -rf $DIR/",
            "$DIR/",
            "`/`, `/*` or a top-level path",
            "rewrite it as `\"${DIR:?}\"/`, which makes the shell stop with an error instead of running rm when $DIR is unset or empty, or use a literal absolute path"
        )
    );
    assert_eq!(
        verdict("rmdir \"$DIR\"/"),
        variable(
            "rmdir",
            "rmdir \"$DIR\"/",
            "\"$DIR\"/",
            "`/`, `/*` or a top-level path",
            "rewrite it as `\"${DIR:?}\"/`, which makes the shell stop with an error instead of running rmdir when $DIR is unset or empty, or use a literal absolute path"
        )
    );
    assert_eq!(
        verdict("rm -rf ${DIR}/*"),
        variable(
            "rm",
            "rm -rf ${DIR}/*",
            "${DIR}/*",
            "`/`, `/*` or a top-level path",
            "rewrite it as `\"${DIR:?}\"/*`, which makes the shell stop with an error instead of running rm when $DIR is unset or empty, or use a literal absolute path"
        )
    );
    assert_eq!(
        verdict("rm -rf ${TMPDIR}/"),
        "ask: Dangerous rm operation detected in `rm -rf ${TMPDIR}/`. The target '${TMPDIR}/' is a shell variable expansion: when $TMPDIR is unset or empty it becomes `/`, `/*` or a top-level path. This requires explicit approval and cannot be auto-allowed by permission rules.\n\nThis check does not fire on a target that cannot expand to the filesystem root: use a literal absolute path: $TMPDIR is normally set, so a guard on it would not stop this rm."
    );
}

#[test]
fn a_variable_in_front_of_a_top_level_directory_asks() {
    assert_eq!(
        verdict("rm -rf $DIR/usr"),
        "ask: Dangerous rm operation detected in `rm -rf $DIR/usr`. The target '$DIR/usr' is a shell variable expansion: when $DIR is unset or empty it becomes `/usr`. This requires explicit approval and cannot be auto-allowed by permission rules.\n\nThis check does not fire on a target that cannot expand to a top-level directory: rewrite it as `\"${DIR:?}\"/usr`, which makes the shell stop with an error instead of removing /usr when $DIR is unset or empty (it is not set in the environment this command inherits), or use a literal absolute path."
    );
}

const WHOLE_SUBSTITUTION: &str = "ask: Dangerous rm operation detected: the target is the output of a command substitution (`$(...)` or backticks) and cannot be checked before the command runs. This requires explicit approval and cannot be auto-allowed by permission rules.\n\nRun the substitution on its own first, then remove the literal paths it prints.";

#[test]
fn a_command_substitution_as_the_target_asks() {
    assert_eq!(verdict("rm -rf $(pwd)"), WHOLE_SUBSTITUTION);
    assert_eq!(verdict("rm -r `pwd`"), WHOLE_SUBSTITUTION);
    assert_eq!(verdict("rm -rf \"$(pwd)\""), WHOLE_SUBSTITUTION);
    // 没有 -r：只是太复杂，不算危险删除。
    assert_eq!(
        verdict("rm -f $(cat list)"),
        "ask: A nested command in this command can't be checked before it runs"
    );
    // 目标以替换结尾：去掉替换后看前面的路径。
    assert_eq!(
        verdict("rm -rf /srv/repos/proj/$(echo x)"),
        literal("rm", "/srv/repos/proj/", WORKSPACE)
    );
    // 替换里面的删除也查。
    assert_eq!(
        verdict("echo \"$(rm -rf ~)\""),
        literal("rm", "/home/dev", CRITICAL)
    );
}

#[test]
fn the_question_is_immune_to_bypass_and_allow_rules() {
    for (mode, entries) in [
        (PermissionMode::BypassPermissions, &[][..]),
        (
            PermissionMode::Default,
            &[(RuleBehavior::Allow, "Bash(rm:*)")][..],
        ),
        (PermissionMode::AcceptEdits, &[][..]),
    ] {
        let result = outcome(mode, entries, "ls && rm -rf .");
        assert!(
            matches!(
                &result,
                Check::Ask {
                    kind: AskKind::Immune,
                    message,
                    ..
                } if message.starts_with("Dangerous rm operation detected: '/srv/repos/proj'")
            ),
            "{mode:?}: {result:?}"
        );
    }
    // deny 规则仍然优先。
    assert_eq!(
        message(&outcome(
            PermissionMode::BypassPermissions,
            &[(RuleBehavior::Deny, "Bash(rm:*)")],
            "rm -rf ."
        )),
        "deny: Permission to use Bash with command rm -rf . has been denied."
    );
}

#[test]
fn a_backslash_only_target_is_the_drive_root_on_windows() {
    let windows = PathContext {
        cwd: "C:\\work\\proj".to_owned(),
        roots: vec!["C:\\work\\proj".to_owned()],
        home: Some("C:\\Users\\dev".to_owned()),
        style: PathStyle::Windows,
        internal: Vec::new(),
        plans: None,
    };
    let result = run_in(&windows, PermissionMode::BypassPermissions, &[], |inputs| {
        message(&check("rm -rf '\\'", inputs))
    });
    assert_eq!(
        result,
        format!(
            "ask: Dangerous rm operation detected: '\\'\n\nA backslash-only target is the drive root in Git Bash on Windows. {TAIL}\n\nWrite the directory you mean as a path, for example /c/work/build or C:/work/build."
        )
    );
    let result = run_in(&windows, PermissionMode::BypassPermissions, &[], |inputs| {
        message(&check("rm -rf C:/work", inputs))
    });
    assert_eq!(result, literal("rm", "C:/work", CRITICAL));
}
