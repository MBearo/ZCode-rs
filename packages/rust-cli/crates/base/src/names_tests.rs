use super::*;

#[test]
fn the_names_keep_their_values() {
    // F55 只收拢定义的位置，名字的值不变（与 Node CLI、App 共用的目录与文件）。
    assert_eq!(DOT_DIR, ".zcode");
    assert_eq!(crate::dot_dir!(), DOT_DIR);
    assert_eq!(
        concat!("~/", crate::dot_dir!(), "/cli/db/db.sqlite"),
        "~/.zcode/cli/db/db.sqlite"
    );
    assert_eq!(RULES_FILE, "AGENTS.md");
    assert_eq!(LOCAL_RULES_FILE, "AGENTS.local.md");
    assert_eq!(WORKTREES_DIR, "worktrees");
    assert_eq!(AGENTS_DIR, "agents");
    assert_eq!(SCHEDULED_TASKS_FILE, "scheduled_tasks.json");
    assert_eq!(CLIENT_NAME, "zcode");
    assert_eq!(OFFICIAL_MARKETPLACE, "zcode-plugins-official");
}

#[test]
fn the_variable_names_keep_their_spelling() {
    use super::vars::*;
    assert_eq!(
        [
            ZCODE_PLUGIN_ROOT,
            CLAUDE_PLUGIN_ROOT,
            ZCODE_PROJECT_DIR,
            CLAUDE_PROJECT_DIR
        ],
        [
            "ZCODE_PLUGIN_ROOT",
            "CLAUDE_PLUGIN_ROOT",
            "ZCODE_PROJECT_DIR",
            "CLAUDE_PROJECT_DIR"
        ]
    );
    assert_eq!(
        [ZCODE_SESSION_ID, CLAUDE_SESSION_ID, CLAUDE_CODE_SESSION_ID],
        [
            "ZCODE_SESSION_ID",
            "CLAUDE_SESSION_ID",
            "CLAUDE_CODE_SESSION_ID"
        ]
    );
    assert_eq!(
        [ZCODE_SKILL_DIR, CLAUDE_SKILL_DIR],
        ["ZCODE_SKILL_DIR", "CLAUDE_SKILL_DIR"]
    );
    assert_eq!(placeholder(CLAUDE_PLUGIN_ROOT), "${CLAUDE_PLUGIN_ROOT}");
}

#[test]
fn the_managed_directory_follows_the_node_platform_name() {
    assert_eq!(managed_dir("darwin"), "/Library/Application Support/ZCode");
    assert_eq!(managed_dir("win32"), r"C:\ProgramData\ZCode");
    assert_eq!(managed_dir("linux"), "/etc/zcode");
    assert_eq!(managed_dir("freebsd"), "/etc/zcode");
}
