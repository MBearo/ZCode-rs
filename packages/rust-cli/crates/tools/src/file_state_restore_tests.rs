use rust_cli_kernel_api::env::EnvFacts;
use rust_cli_kernel_api::ids::{EnvironmentId, MessageId};
use rust_cli_kernel_api::message::{AssistantMessage, UserMessage, UserOrigin};
use rust_cli_kernel_api::testing::{MemoryEnvironment, UnusedEnvironment};
use serde_json::json;

use super::*;

/// 一次成功的 Write：调用与它的结果。
fn written(path: &str) -> Vec<Message> {
    vec![
        Message::Assistant(AssistantMessage {
            id: MessageId::new("a"),
            content: vec![ContentBlock::ToolUse {
                id: ToolUseId::new("t1"),
                name: write::NAME.to_owned(),
                input: json!({ "file_path": path, "content": "one\n" }),
            }],
            model: "m".to_owned(),
            stop_reason: None,
            usage: None,
        }),
        Message::User(UserMessage {
            id: MessageId::new("u"),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: ToolUseId::new("t1"),
                content: vec![ToolResultContent::Text {
                    text: "written".to_owned(),
                }],
                is_error: false,
            }],
            origin: UserOrigin::ToolResult {
                assistant: MessageId::new("a"),
            },
        }),
    ]
}

/// F19：冷恢复的相对路径按工作区根解析，shell 的当前目录在别处也一样（基准是启动时的目录，
/// tools-files.md §3.3、§4）。
#[tokio::test]
async fn restored_relative_paths_use_the_workspace_root() {
    let session = SessionId::new("s");
    let env = MemoryEnvironment::new(EnvFacts {
        cwd: EnvPath::new("/w"),
        platform: "darwin".into(),
        os_version: "x".into(),
        shell: "zsh".into(),
        is_git_repo: false,
        additional_directories: Vec::new(),
        home: None,
    });
    env.set_shell_cwd("/w/sub");
    let workspace = WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new("/w"),
    };
    let state = ReadFileState::default();
    restore_file_state(&state, &session, &written("x.txt"), &env, &workspace).await;
    assert!(state.get(&session, "/w/x.txt").is_some());
    assert!(state.get(&session, "/w/sub/x.txt").is_none());
}

/// F14：冷恢复的键与工具记下的键同一规则（都经文件工具的路径解析，
/// tools-files.md §3.2）。修前路径原样解析：带空白、分解形式的 `file_path` 恢复成另一个键，续写后的 Edit
/// 报“没读过”。
#[tokio::test]
async fn restored_keys_follow_the_tool_path_rule() {
    let session = SessionId::new("s");
    let messages = vec![
        Message::Assistant(AssistantMessage {
            id: MessageId::new("a"),
            content: vec![ContentBlock::ToolUse {
                id: ToolUseId::new("t1"),
                name: write::NAME.to_owned(),
                input: json!({ "file_path": " /w/cafe\u{301}.txt\n", "content": "one\n" }),
            }],
            model: "m".to_owned(),
            stop_reason: None,
            usage: None,
        }),
        Message::User(UserMessage {
            id: MessageId::new("u"),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: ToolUseId::new("t1"),
                content: vec![ToolResultContent::Text {
                    text: "written".to_owned(),
                }],
                is_error: false,
            }],
            origin: UserOrigin::ToolResult {
                assistant: MessageId::new("a"),
            },
        }),
    ];
    let state = ReadFileState::default();
    let workspace = WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new("/w"),
    };
    restore_file_state(&state, &session, &messages, &UnusedEnvironment, &workspace).await;
    let restored = state.get(&session, "/w/caf\u{e9}.txt");
    assert_eq!(restored.map(|file| file.content), Some("one\n".to_owned()));
}
