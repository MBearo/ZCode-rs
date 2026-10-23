use rust_cli_kernel_api::env::{EnvFacts, EnvPath};
use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::message::{AttachmentMessage, Message};
use serde_json::Value;

use super::*;

fn facts(cwd: &str, git: bool, platform: &str) -> EnvFacts {
    EnvFacts {
        cwd: EnvPath::new(cwd),
        platform: platform.to_owned(),
        os_version: "Darwin 24.6.0".to_owned(),
        shell: "zsh".to_owned(),
        is_git_repo: git,
        additional_directories: Vec::new(),
        home: None,
    }
}

fn announced((text, data): (String, Value)) -> Message {
    Message::Attachment(AttachmentMessage {
        id: MessageId::new("a"),
        source: SOURCE.to_owned(),
        text,
        data,
    })
}

#[test]
fn the_first_announcement_is_the_whole_block() {
    let (text, data) = announce(&[], &facts("/w/<a>", false, "darwin")).unwrap();
    assert!(text.starts_with(
        "# Environment\nYou have been invoked in the following environment: \n - Primary working directory: /w/&lt;a&gt;\n - Is a git repository: false\n - Platform: darwin\n - Shell: zsh\n - OS Version: Darwin 24.6.0\n - Downloaded files"
    ));
    assert_eq!(data["snapshot"]["workingDirectory"], "/w/<a>");
    assert!(data.get("changes").is_none());
}

#[test]
fn unchanged_facts_are_not_announced_again_and_changes_are_an_update() {
    let first = announced(announce(&[], &facts("/w", false, "darwin")).unwrap());
    assert!(announce(std::slice::from_ref(&first), &facts("/w", false, "darwin")).is_none());

    let (text, data) =
        announce(std::slice::from_ref(&first), &facts("/v", true, "darwin")).unwrap();
    assert_eq!(
        text,
        "# Environment update\n - Primary working directory: /v (was /w)\n - Is a git repository: true (was false)"
    );
    assert_eq!(data["changes"][0]["field"], "workingDirectory");

    // 平台变了（换了执行环境）就重发整块。
    let (text, _) = announce(&[first], &facts("/w", false, "linux")).unwrap();
    assert!(text.starts_with("# Environment\n"));
}

#[test]
fn additional_directories_are_nested_items() {
    let mut current = facts("/w", false, "darwin");
    current.additional_directories = vec![EnvPath::new("/x")];
    let (text, _) = announce(&[], &current).unwrap();
    assert!(text.contains(" - Additional working directories:\n  - /x\n - Platform: darwin"));
}
