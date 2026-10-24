use rust_cli_kernel_api::env::{EnvFacts, EnvPath, WorkspaceRef};
use rust_cli_kernel_api::env_files::PathStyle;
use rust_cli_kernel_api::ids::{EnvironmentId, MessageId, ToolUseId};
use rust_cli_kernel_api::message::{
    AssistantMessage, AttachmentMessage, ToolResultContent, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::testing::MemoryEnvironment;

use super::*;

fn assistant(calls: &[(&str, &str, Value)]) -> Message {
    Message::Assistant(AssistantMessage {
        id: MessageId::new("a"),
        content: calls
            .iter()
            .map(|(id, name, input)| ContentBlock::ToolUse {
                id: ToolUseId::new(*id),
                name: (*name).to_owned(),
                input: input.clone(),
            })
            .collect(),
        model: "m".to_owned(),
        stop_reason: None,
        usage: None,
    })
}

fn results(answers: &[(&str, bool)]) -> Message {
    Message::User(UserMessage {
        id: MessageId::new("u"),
        content: answers
            .iter()
            .map(|(id, is_error)| ContentBlock::ToolResult {
                tool_use_id: ToolUseId::new(*id),
                content: vec![ToolResultContent::Text {
                    text: "ok".to_owned(),
                }],
                is_error: *is_error,
            })
            .collect(),
        origin: UserOrigin::ToolResult {
            assistant: MessageId::new("a"),
        },
    })
}

fn path(file: &str) -> Value {
    json!({ "file_path": file })
}

#[test]
fn triggers_are_the_successful_file_calls_of_the_last_response() {
    let history = vec![
        assistant(&[("t0", "Read", path("/w/old.rs"))]),
        results(&[("t0", false)]),
        assistant(&[
            ("t1", "Read", path("/w/src/a.rs")),
            ("t2", "Edit", path("/w/src/b.rs")),
            ("t3", "Write", path("/w/src/c.rs")),
            ("t4", "Read", path("/w/src/failed.rs")),
            ("t5", "NotebookEdit", json!({"notebook_path": "/w/n.ipynb"})),
            ("t6", "Grep", path("/w/src/d.rs")),
            ("t7", "Read", path("/w/src/a.rs")),
        ]),
        results(&[
            ("t1", false),
            ("t2", false),
            ("t3", false),
            ("t4", true),
            ("t5", false),
            ("t6", false),
            ("t7", false),
        ]),
    ];
    assert_eq!(
        triggers(&history),
        ["/w/src/a.rs", "/w/src/b.rs", "/w/src/c.rs"]
    );
    // 结果还没回来的调用不算。
    assert!(triggers(&history[..3]).is_empty());
}

fn facts() -> EnvFacts {
    EnvFacts {
        cwd: EnvPath::new("/w"),
        platform: "linux".to_owned(),
        os_version: String::new(),
        shell: "bash".to_owned(),
        is_git_repo: false,
        additional_directories: Vec::new(),
        home: Some(EnvPath::new("/Users/dev")),
    }
}

fn workspace() -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new("/w"),
    }
}

fn recorded((text, data): (String, Value)) -> Message {
    Message::Attachment(AttachmentMessage {
        id: MessageId::new("n"),
        source: SOURCE.to_owned(),
        text,
        data,
    })
}

/// 启动时已宣布的规则（`instructions` 记录）。
fn announced_rules(files: &[RuleFile]) -> Message {
    Message::Attachment(AttachmentMessage {
        id: MessageId::new("i"),
        source: instructions::SOURCE.to_owned(),
        text: String::new(),
        data: json!({ "files": instructions::current(files) }),
    })
}

#[tokio::test]
async fn directory_rules_are_announced_once_per_file() {
    let env = MemoryEnvironment::new(facts())
        .with_file("/w/src/AGENTS.md", "src rules\n")
        .with_file("/w/src/.zcode/AGENTS.md", "same as announced")
        .with_file("/w/src/ui/AGENTS.md", "ui rules");
    let ws = workspace();
    let scope = Scope {
        files: &env,
        workspace: &ws,
        style: PathStyle::Posix,
        cwd: "/w".to_owned(),
        home: Some("/Users/dev".to_owned()),
    };
    // 当前规则里有 `.zcode/AGENTS.md`，并且已经以同样的内容宣布过。
    let files = [RuleFile {
        path: "/w/src/.zcode/AGENTS.md".to_owned(),
        kind: crate::rules::Kind::Project,
        content: "same as announced".to_owned(),
        globs: None,
        parent: None,
    }];
    let mut history = vec![
        announced_rules(&files),
        assistant(&[
            ("t1", "Read", path("/w/src/a.rs")),
            ("t2", "Read", path("/elsewhere/x.rs")),
        ]),
        results(&[("t1", false), ("t2", false)]),
    ];
    let seen = Seen::default();
    let session = SessionId::new("sess_1");
    let found = announce((&scope, &seen, &session, &env), &facts(), &history, &files).await;
    assert_eq!(found.len(), 1);
    let (text, data) = &found[0];
    // 内容不 trim，没有 label。
    assert_eq!(text, "Contents of /w/src/AGENTS.md:\n\nsrc rules\n");
    assert_eq!(
        data,
        &json!({
            "path": "/w/src/AGENTS.md",
            "content": {"path": "/w/src/AGENTS.md", "type": "Project", "content": "src rules\n"},
            "displayPath": "src/AGENTS.md",
        })
    );

    // 下一次触及更深的文件：已发过的不再发，只发新目录的。
    history.push(recorded(found[0].clone()));
    history.push(assistant(&[("t3", "Edit", path("src/ui/button.tsx"))]));
    history.push(results(&[("t3", false)]));
    let seen = Seen::default();
    let session = SessionId::new("sess_1");
    let found = announce((&scope, &seen, &session, &env), &facts(), &history, &files).await;
    assert_eq!(
        found
            .iter()
            .map(|(_, data)| data["path"].clone())
            .collect::<Vec<_>>(),
        [json!("/w/src/ui/AGENTS.md")]
    );

    // 模型自己读过的规则文件也不再发。
    let history = vec![
        announced_rules(&files),
        assistant(&[
            ("t1", "Read", path("/w/src/AGENTS.md")),
            ("t2", "Read", path("/w/src/ui/x.rs")),
        ]),
        results(&[("t1", false), ("t2", false)]),
    ];
    let seen = Seen::default();
    let session = SessionId::new("sess_1");
    let found = announce((&scope, &seen, &session, &env), &facts(), &history, &files).await;
    assert_eq!(
        found
            .iter()
            .map(|(_, data)| data["path"].clone())
            .collect::<Vec<_>>(),
        [json!("/w/src/ui/AGENTS.md")]
    );
}

/// F14：触发路径与工具读的是同一个文件（触发是工具解析后的路径，tools-files.md
/// §3.2）：带首尾空白的路径也找到它所在目录的规则；修前 `" /w/src/a.rs"` 被当成相对路径，落在工作区外的
/// `/w/ /w/src`，没有规则。
#[tokio::test]
async fn trigger_paths_are_resolved_like_the_tools_resolve_them() {
    let env = MemoryEnvironment::new(facts()).with_file("/w/src/AGENTS.md", "src rules\n");
    let ws = workspace();
    let scope = Scope {
        files: &env,
        workspace: &ws,
        style: PathStyle::Posix,
        cwd: "/w".to_owned(),
        home: Some("/Users/dev".to_owned()),
    };
    let history = vec![
        assistant(&[("t1", "Read", path(" /w/src/a.rs\n"))]),
        results(&[("t1", false)]),
    ];
    let found = announce(
        (&scope, &Seen::default(), &SessionId::new("sess_1"), &env),
        &facts(),
        &history,
        &[],
    )
    .await;
    assert_eq!(
        found
            .iter()
            .map(|(_, data)| data["path"].clone())
            .collect::<Vec<_>>(),
        [json!("/w/src/AGENTS.md")]
    );
}

/// F19：相对的触发路径按会话的当前目录解析（执行环境的 shell cwd，触发是工具调用时当前目录下的绝对
/// 路径，tools-files.md §3.3）：Bash `cd src` 之后 Read `a.rs` 读的是 `/w/src/a.rs`，带出 `/w/src/AGENTS.md`。修前按
/// 工作区根解析成 `/w/a.rs`，没有规则。范围仍按工作区根：`cd` 到工作区外（执行侧会改回，这里直接设）也不越界。
#[tokio::test]
async fn relative_triggers_follow_the_session_directory() {
    let env = MemoryEnvironment::new(facts())
        .with_file("/w/src/AGENTS.md", "src rules\n")
        .with_file("/x/AGENTS.md", "outside\n");
    env.set_shell_cwd("/w/src");
    let ws = workspace();
    let scope = Scope {
        files: &env,
        workspace: &ws,
        style: PathStyle::Posix,
        cwd: "/w".to_owned(),
        home: None,
    };
    let history = vec![
        assistant(&[("t1", "Read", path("a.rs"))]),
        results(&[("t1", false)]),
    ];
    let session = SessionId::new("sess_1");
    let found = announce(
        (&scope, &Seen::default(), &session, &env),
        &facts(),
        &history,
        &[],
    )
    .await;
    assert_eq!(
        found
            .iter()
            .map(|(_, data)| (data["path"].clone(), data["displayPath"].clone()))
            .collect::<Vec<_>>(),
        // displayPath 仍相对工作区根（不进模型请求，§3.3 的登记）。
        [(json!("/w/src/AGENTS.md"), json!("src/AGENTS.md"))]
    );
    env.set_shell_cwd("/x");
    let found = announce(
        (&scope, &Seen::default(), &session, &env),
        &facts(),
        &history,
        &[],
    )
    .await;
    assert!(found.is_empty(), "{found:?}");
}

/// 带唯一 id 的消息（增量缓存按 id 核对前缀）。
fn with_id(mut message: Message, id: &str) -> Message {
    match &mut message {
        Message::User(user) => user.id = MessageId::new(id),
        Message::Assistant(assistant) => assistant.id = MessageId::new(id),
        Message::Attachment(attachment) => attachment.id = MessageId::new(id),
        Message::System(_) => {}
    }
    message
}

/// 按会话增量算的“触及过与发过的路径”与从头算的相同（修复线 F6）：逐条追加历史，每一步都比；结果晚到、失败的
/// 结果、同一调用后来又失败、相对路径与 `~`、发过的 nested memory；换了前缀（压缩、回退）或工作区时从头算。
#[test]
fn the_incremental_known_set_matches_a_full_scan_at_every_step() {
    let env = MemoryEnvironment::new(facts());
    let ws = workspace();
    let scope = |cwd: &str| Scope {
        files: &env,
        workspace: &ws,
        style: PathStyle::Posix,
        cwd: cwd.to_owned(),
        home: Some("/Users/dev".to_owned()),
    };
    let noted = |path: &str| {
        recorded((
            String::new(),
            json!({"path": path, "content": {}, "displayPath": path}),
        ))
    };
    let history = vec![
        with_id(
            assistant(&[
                ("t1", "Read", path("src/a.rs")),
                ("t2", "Write", path("~/b.rs")),
            ]),
            "m1",
        ),
        with_id(results(&[("t1", false)]), "m2"),
        with_id(
            assistant(&[
                ("t3", "Edit", path("/w/c.rs")),
                ("t4", "Grep", path("/w/d.rs")),
            ]),
            "m3",
        ),
        with_id(results(&[("t2", false), ("t3", true), ("t4", false)]), "m4"),
        with_id(noted("/w/src/AGENTS.md"), "m5"),
        // 同一个调用后来又有一个失败的结果：不再算触及过。
        with_id(results(&[("t1", true)]), "m6"),
        with_id(
            assistant(&[
                ("t5", "Read", path("/w/c.rs")),
                ("t5", "Read", path("/w/e.rs")),
            ]),
            "m7",
        ),
        with_id(results(&[("t5", false)]), "m8"),
    ];
    let seen = Seen::default();
    let session = SessionId::new("sess_1");
    for end in 0..=history.len() {
        let scope = scope("/w");
        let mut expected: Vec<_> = already_known(&scope, &history[..end]).into_iter().collect();
        let mut got: Vec<_> = seen
            .known(&session, &scope, &history[..end])
            .into_iter()
            .collect();
        expected.sort();
        got.sort();
        assert_eq!(got, expected, "after {end} messages");
    }
    // 压缩之后（新的第一条）、回退之后（更短）、换了工作区：从头算。
    let compacted = history[4..].to_vec();
    let rewound = history[..3].to_vec();
    for (cwd, history) in [("/w", &compacted), ("/w", &rewound), ("/other", &history)] {
        let scope = scope(cwd);
        let mut expected: Vec<_> = already_known(&scope, history).into_iter().collect();
        let mut got: Vec<_> = seen.known(&session, &scope, history).into_iter().collect();
        expected.sort();
        got.sort();
        assert_eq!(got, expected, "{cwd} {}", history.len());
    }
}
