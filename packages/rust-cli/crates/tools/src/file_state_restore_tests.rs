use rust_cli_kernel_api::env::EnvFacts;
use rust_cli_kernel_api::ids::MachineName;
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
        home: None,
        uid: None,
    });
    env.set_shell_cwd("/w/sub");
    let workspace = WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new("/w"),
    };
    let state = ReadFileState::default();
    restore_file_state(&state, &session, &written("x.txt"), &env, &workspace).await;
    assert!(
        state
            .get_at(&session, &MachineName::primary(), "/w/x.txt")
            .is_some()
    );
    assert!(
        state
            .get_at(&session, &MachineName::primary(), "/w/sub/x.txt")
            .is_none()
    );
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
    let restored = state.get_at(&session, &MachineName::primary(), "/w/caf\u{e9}.txt");
    assert_eq!(restored.map(|file| file.content), Some("one\n".to_owned()));
}

/// 冷恢复的 Read 记录同实时的 Read：`offset` 是 1（工作线 F130，D4 接线之后续写的场景报出）。读过的文件在磁盘上
/// 被改动的检查只看没有 offset、也没有 limit 的记录（tools-files.md §4.1），所以续写之后没改过的文件不会被报成“改过”。
/// 以前恢复出的 Read 记录没有 offset，文件内容（带末尾换行）与去掉行号的结果文字（去掉了末尾空白）一比就不同。
#[tokio::test]
async fn a_restored_read_is_not_checked_for_changes_on_disk() {
    let session = SessionId::new("s");
    let messages = vec![
        Message::Assistant(AssistantMessage {
            id: MessageId::new("a"),
            content: vec![ContentBlock::ToolUse {
                id: ToolUseId::new("t1"),
                name: read::NAME.to_owned(),
                input: json!({ "file_path": "/w/first.txt" }),
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
                    text: "     1\talpha\n".to_owned(),
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
    let restored = state
        .get_at(&session, &MachineName::primary(), "/w/first.txt")
        .unwrap();
    assert_eq!(restored.offset, Some(1));
    assert_eq!(restored.limit, None);
    assert!(restored.is_full_view());
    assert!(state.whole_files_everywhere(&session).is_empty());
    // 结果文字去掉了末尾换行：没改过的文件（`alpha\n`）不算读后被改过，改过的算。
    let version = rust_cli_kernel_api::env::FileVersion::new("v1");
    assert!(!restored.changed(&version, || Some("alpha\n".to_owned())));
    assert!(restored.changed(&version, || Some("alpha beta\n".to_owned())));
}
