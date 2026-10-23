use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::ids::{EnvironmentId, MessageId};
use rust_cli_kernel_api::message::AttachmentMessage;
use rust_cli_kernel_api::testing::{MemoryEnvironment, UnusedEnvironment};

use super::*;

fn snapshot(status: &str, user: Option<&str>) -> GitSnapshot {
    GitSnapshot {
        branch: "feat/rules".to_owned(),
        default_branch: "main".to_owned(),
        status: status.to_owned(),
        recent_commits: "abc1234 second\ndef5678 first".to_owned(),
        user_name: user.map(str::to_owned),
    }
}

fn facts(git: bool) -> EnvFacts {
    EnvFacts {
        cwd: EnvPath::new("/w"),
        platform: "linux".to_owned(),
        os_version: String::new(),
        shell: "bash".to_owned(),
        is_git_repo: git,
        additional_directories: Vec::new(),
        home: None,
    }
}

fn workspace() -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new("/w"),
    }
}

const HEADER: &str = "This is the git status at the start of the conversation. Note that this status is a snapshot in time, and will not update during the conversation.";

#[test]
fn the_git_status_has_all_its_sections() {
    assert_eq!(
        git_text(&snapshot("M src/a.rs", Some("Dev"))),
        format!(
            "{HEADER}\n\nCurrent branch: feat/rules\n\nMain branch (you will usually use this for PRs): main\n\n\
             Git user: Dev\n\nStatus:\nM src/a.rs\n\nRecent commits:\nabc1234 second\ndef5678 first"
        )
    );
    // 没有 user.name 时整段不出现；没有改动时是 (clean)。
    let clean = git_text(&snapshot("", None));
    assert!(!clean.contains("Git user:"));
    assert!(clean.contains("\n\nStatus:\n(clean)\n\n"));
}

#[test]
fn long_status_is_cut_at_2000_utf16_units() {
    let status = "é".repeat(1999) + "😀" + "tail";
    let text = git_text(&snapshot(&status, None));
    let expected = format!(
        "Status:\n{}\n... (truncated because it exceeds 2k characters. If you need more information, run \"git status\" using Bash)\n\n",
        "é".repeat(1999)
    );
    assert!(text.contains(&expected), "{text}");
    assert!(
        git_text(&snapshot(&"x".repeat(2000), None))
            .contains(&format!("Status:\n{}\n\n", "x".repeat(2000)))
    );
}

#[tokio::test]
async fn the_context_is_announced_once_in_a_git_repository() {
    let env = MemoryEnvironment::new(facts(true))
        .with_git(GitStatus::Repository(snapshot("", Some("Dev"))));
    let (text, data) = announce(&env, &workspace(), &facts(true), &[])
        .await
        .unwrap();
    let git = git_text(&snapshot("", Some("Dev")));
    assert_eq!(
        text,
        format!(
            "As you answer the user's questions, you can use the following context:\n# gitStatus\n{git}\n\n\
             ZCode attached this context automatically; it isn't part of the user's message. It describes the user's own account and workspace, so they don't need it reported back."
        )
    );
    assert_eq!(data, json!({"context": {"gitStatus": git}}));

    // 边界窗口里宣布过就沿用，不再取 git 状态。
    let history = vec![Message::Attachment(AttachmentMessage {
        id: MessageId::new("m"),
        source: SOURCE.to_owned(),
        text,
        data,
    })];
    assert!(
        announce(&UnusedEnvironment, &workspace(), &facts(true), &history)
            .await
            .is_none()
    );
}

#[tokio::test]
async fn no_context_outside_git_or_when_the_environment_cannot_tell() {
    let env =
        MemoryEnvironment::new(facts(false)).with_git(GitStatus::Repository(snapshot("", None)));
    assert!(
        announce(&env, &workspace(), &facts(false), &[])
            .await
            .is_none()
    );
    let not_repo = MemoryEnvironment::new(facts(true)).with_git(GitStatus::NotRepository);
    assert!(
        announce(&not_repo, &workspace(), &facts(true), &[])
            .await
            .is_none()
    );
    // 执行环境不支持 git 状态。
    let unsupported = MemoryEnvironment::new(facts(true));
    assert!(
        announce(&unsupported, &workspace(), &facts(true), &[])
            .await
            .is_none()
    );
}
