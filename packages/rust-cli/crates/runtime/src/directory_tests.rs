//! `/cd` 的路径检查与换目录（tui-manage.md §7）。
use std::sync::Arc;

use rust_cli_kernel_api::env::{EnvFacts, WorkspaceRef};
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::message::{ContentBlock, Message, UserOrigin};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{CreateSession, Kernel};
use rust_cli_kernel_api::testing::{
    FixedClock, MemoryEnvironment, MemoryStore, ScriptedModel, SequentialIds, environments,
    selection,
};

use super::*;
use crate::{Defaults, Ports};

fn kernel() -> (LocalKernel, Arc<MemoryEnvironment>) {
    let env = Arc::new(
        MemoryEnvironment::new(EnvFacts {
            cwd: EnvPath::new("/Users/dev/project"),
            platform: "darwin".to_owned(),
            os_version: "x".to_owned(),
            shell: "zsh".to_owned(),
            is_git_repo: false,
            additional_directories: Vec::new(),
            home: Some(EnvPath::new("/Users/dev")),
        })
        .with_file("/Users/dev/project/AGENTS.md", "project rules")
        .with_file("/Users/dev/other/AGENTS.md", "other rules")
        .with_file("/Users/dev/notes.txt", "x"),
    );
    let store = Arc::new(MemoryStore::default());
    let kernel = LocalKernel::new(
        Ports {
            model: Arc::new(ScriptedModel::new(Vec::new())),
            environments: environments(env.clone()),
            store: store.clone(),
            artifacts: store,
            clock: Arc::new(FixedClock(1_000)),
            ids: Arc::new(SequentialIds::default()),
        },
        Defaults {
            selection: selection(),
            mode: PermissionMode::Default,
            max_turns: None,
            search_tools: false,
            task_tools: None,
            limits: Default::default(),
        },
    );
    (kernel, env)
}

fn workspace() -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new("/Users/dev/project"),
    }
}

fn request(path: &str, session: Option<rust_cli_kernel_api::ids::SessionId>) -> ChangeDirectory {
    ChangeDirectory {
        workspace: workspace(),
        session,
        path: path.to_owned(),
    }
}

#[test]
fn display_paths_use_a_tilde_for_home() {
    let home = EnvPath::new("/Users/dev");
    assert_eq!(display("/Users/dev/other", Some(&home)), "~/other");
    assert_eq!(display("/Users/dev", Some(&home)), "~");
    assert_eq!(display("/Users/devx", Some(&home)), "/Users/devx");
    assert_eq!(display("/tmp", None), "/tmp");
}

#[tokio::test]
async fn bad_targets_explain_what_is_wrong() {
    let (kernel, _) = kernel();
    let error = |path: &str| {
        let kernel = &kernel;
        let path = path.to_owned();
        async move {
            kernel
                .change_directory(request(&path, None))
                .await
                .unwrap_err()
                .to_string()
        }
    };
    assert_eq!(
        error("../missing").await,
        "Couldn't find a directory at /Users/dev/missing."
    );
    assert_eq!(
        error("~/notes.txt").await,
        "/Users/dev/notes.txt is not a directory. Did you mean /Users/dev?"
    );
    assert_eq!(error(".").await, "Already in ~/project.");
    // 没有会话：只检查与解析。
    let moved = kernel
        .change_directory(request("../other", None))
        .await
        .unwrap();
    assert_eq!(moved.root.as_str(), "/Users/dev/other");
    assert_eq!(moved.display, "~/other");
}

#[tokio::test]
async fn the_session_moves_and_gets_the_note_with_the_new_rules() {
    let (kernel, env) = kernel();
    let session = kernel
        .create_session(CreateSession {
            id: None,
            workspace: workspace(),
            selection: selection(),
            mode: PermissionMode::Default,
            plan: false,
        })
        .await
        .unwrap();
    let moved = kernel
        .change_directory(request("../other", Some(session.clone())))
        .await
        .unwrap();
    assert_eq!(moved.display, "~/other");
    let view = kernel
        .open_session(&session)
        .await
        .unwrap()
        .view()
        .await
        .unwrap();
    assert_eq!(view.meta.workspace.root.as_str(), "/Users/dev/other");
    let Some(Message::User(note)) = view.messages.last() else {
        panic!("no note: {:?}", view.messages);
    };
    assert_eq!(
        note.origin,
        UserOrigin::Meta {
            source: "rust-cli/cd".to_owned()
        }
    );
    let ContentBlock::Text { text } = &note.content[0] else {
        panic!("not text");
    };
    assert!(text.starts_with("<system-reminder>\nThe session's working directory has changed to /Users/dev/other (via /cd)."), "{text}");
    assert!(
        text.contains("</system-reminder>\n\nCodebase and user instructions are shown below."),
        "{text}"
    );
    assert!(
        // 规则文件只列新目录这一条链上的（`/Users/dev/project` 不在其中）。
        text.contains("other rules") && !text.contains("project rules"),
        "{text}"
    );
    // shell 的当前目录也换了：再 cd 时从新目录起算。
    let state = env.shell_state(&workspace(), &session).await.unwrap();
    assert_eq!(state.cwd.as_str(), "/Users/dev/other");
    // TUI 成功后换上了新工作区。
    let back = kernel
        .change_directory(ChangeDirectory {
            workspace: WorkspaceRef {
                environment: EnvironmentId::new("local"),
                root: moved.root.clone(),
            },
            session: Some(session),
            path: "../project".to_owned(),
        })
        .await
        .unwrap();
    assert_eq!(back.root.as_str(), "/Users/dev/project");
}

/// 复现（R1-3）：以前“已经在这里”按前端给的工作区判定——前端的副本过时（EnterWorktree 之后、或这里故意给旧值）
/// 时，会话已经在目标目录也再移动一次、再提交一条说明。现在由会话 actor 按自己的工作目录判定。
#[tokio::test]
async fn already_there_is_judged_by_the_session_not_the_front_end_copy() {
    let (kernel, _env) = kernel();
    let session = kernel
        .create_session(CreateSession {
            id: None,
            workspace: workspace(),
            selection: selection(),
            mode: PermissionMode::Default,
            plan: false,
        })
        .await
        .unwrap();
    kernel
        .change_directory(request("../other", Some(session.clone())))
        .await
        .unwrap();
    let handle = kernel.open_session(&session).await.unwrap();
    let notes = handle.view().await.unwrap().messages.len();
    // 前端还拿着旧工作区（/Users/dev/project）。
    let again = kernel
        .change_directory(request("/Users/dev/other", Some(session.clone())))
        .await
        .unwrap_err();
    assert_eq!(again.to_string(), "Already in ~/other.");
    assert_eq!(handle.view().await.unwrap().messages.len(), notes);
}

/// `/plan open` 的计划文件（tui-manage.md §8）：与 ExitPlanMode 同一个位置，空文件不算有计划。
#[tokio::test]
async fn the_plan_file_is_the_node_location() {
    use rust_cli_kernel_api::manage::PlanFileQuery;
    let (kernel, env) = kernel();
    let session = rust_cli_kernel_api::ids::SessionId::new("ses_1");
    let query = PlanFileQuery {
        workspace: workspace(),
        session: session.clone(),
    };
    let file = kernel.plan_file(query.clone()).await.unwrap();
    assert_eq!(file.path, "/Users/dev/project/.zcode/plans/plan-ses_1.md");
    assert!(!file.exists);
    env.set_file(&file.path, "");
    assert!(!kernel.plan_file(query.clone()).await.unwrap().exists);
    env.set_file(&file.path, "# Plan\n");
    assert!(kernel.plan_file(query).await.unwrap().exists);
}
