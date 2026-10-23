//! EnterWorktree / ExitWorktree 的校验、权限、结果文本与工作目录的切换（tools-more.md §11）。

use std::sync::{Arc, PoisonError};

use rust_cli_kernel_api::env_worktree::WorktreeChanges;
use rust_cli_kernel_api::permission::PermissionDecision;
use rust_cli_kernel_api::tool::{Tool, ToolUseContext, Validation};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::*;
use crate::test_more::{Ctx, output_text};
use crate::worktree_exit::EXIT;

#[path = "worktree_tests_fakes.rs"]
mod fakes;
use fakes::{Git, ROOT, Session, WINDOWS_ROOT};

struct Fixture {
    tools: Vec<Arc<dyn Tool>>,
    session: Arc<Session>,
    env: Arc<Git>,
    /// 工作区根；None 时用 `Ctx` 的 `/Users/dev/work`。
    root: Option<&'static str>,
}

impl Fixture {
    fn new(env: Git) -> Self {
        Self {
            tools: worktree_tools(&Arc::default()),
            session: Arc::default(),
            env: Arc::new(env),
            root: None,
        }
    }

    fn context(&self) -> ToolUseContext {
        let mut ctx = Ctx::new(self.session.clone());
        ctx.env = self.env.clone();
        let mut context = ctx.build();
        if let Some(root) = self.root {
            context.workspace.root = EnvPath::new(root);
        }
        context
    }

    fn tool(&self, name: &str) -> &Arc<dyn Tool> {
        self.tools
            .iter()
            .find(|tool| tool.name() == name)
            .unwrap_or_else(|| panic!("{name}"))
    }

    async fn validate(&self, name: &str, input: Value) -> Validation {
        self.tool(name)
            .validate_input(&input, &self.context())
            .await
    }

    async fn call(&self, name: &str, input: Value) -> Result<String, String> {
        let (progress, _rx) = mpsc::channel(1);
        self.tool(name)
            .call(input, self.context(), progress)
            .await
            .map(|output| output_text(&output))
            .map_err(|error| error.to_string())
    }

    fn switches(&self) -> Vec<String> {
        self.session
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

fn message(validation: Validation) -> (String, Option<i32>) {
    match validation {
        Validation::Invalid {
            message,
            error_code,
        } => (message, error_code),
        Validation::Ok => panic!("expected an error"),
    }
}

#[tokio::test]
async fn worktree_names_are_checked() {
    let fixture = Fixture::new(Git::new());
    for (name, expected) in [
        (
            "a/../b",
            "Invalid worktree name \"a/../b\": must not contain \".\" or \"..\" path segments",
        ),
        (
            "x/.git.",
            "Invalid worktree name \"x/.git.\": \".git.\" is a reserved git directory name",
        ),
        (
            "a b",
            "Invalid worktree name \"a b\": each \"/\"-separated segment must be non-empty and contain only letters, digits, dots, underscores, and dashes",
        ),
        (
            "a//b",
            "Invalid worktree name \"a//b\": each \"/\"-separated segment must be non-empty and contain only letters, digits, dots, underscores, and dashes",
        ),
    ] {
        assert_eq!(
            message(fixture.validate(ENTER, json!({"name": name})).await).0,
            expected
        );
    }
    let long = "a".repeat(65);
    assert_eq!(
        message(fixture.validate(ENTER, json!({"name": long})).await).0,
        "Invalid worktree name: must be 64 characters or fewer (got 65)"
    );
    assert_eq!(
        message(
            fixture
                .validate(ENTER, json!({"name": "a", "path": "b"}))
                .await
        )
        .0,
        texts::BOTH_GIVEN
    );
}

#[tokio::test]
async fn create_switches_the_session_and_exit_keeps_it() {
    let fixture = Fixture::new(Git::new());
    assert_eq!(
        message(fixture.validate(EXIT, json!({"action": "keep"})).await),
        (texts::NO_SESSION.to_owned(), Some(1))
    );
    let text = fixture
        .call(ENTER, json!({"name": "user/feature"}))
        .await
        .unwrap();
    assert_eq!(
        text,
        "Created worktree at /Users/dev/work/.zcode/worktrees/user+feature on branch worktree-user+feature. The session is now working in the worktree. Use ExitWorktree to leave mid-session, or exit the session to be prompted."
    );
    assert_eq!(
        fixture.switches(),
        ["/Users/dev/work/.zcode/worktrees/user+feature"]
    );
    assert_eq!(
        message(fixture.validate(ENTER, json!({"name": "again"})).await),
        (texts::ALREADY_IN.to_owned(), Some(2))
    );
    let text = fixture.call(EXIT, json!({"action": "keep"})).await.unwrap();
    assert_eq!(
        text,
        "Exited worktree. Your work is preserved at /Users/dev/work/.zcode/worktrees/user+feature on branch worktree-user+feature. Session is now back in /Users/dev/work."
    );
    assert_eq!(fixture.switches()[1], ROOT);
    assert!(fixture.validate(EXIT, json!({"action": "keep"})).await != Validation::Ok);
}

#[tokio::test]
async fn resumed_and_default_names() {
    let mut git = Git::new();
    git.existed = true;
    let fixture = Fixture::new(git);
    let text = fixture.call(ENTER, json!({})).await.unwrap();
    assert_eq!(
        text,
        format!(
            // 缺省名是会话 `sess_1` 的词组（tools-more.md §11.6）。
            "Resumed worktree at /Users/dev/work/.zcode/worktrees/lazy-foraging-planet on branch worktree-lazy-foraging-planet.{} The session is now working in the worktree. Use ExitWorktree to leave mid-session, or exit the session to be prompted.",
            texts::RESUMED_NOTE
        )
    );
}

#[tokio::test]
async fn remove_refuses_unsaved_work_until_confirmed() {
    let mut git = Git::new();
    git.changes = Some(WorktreeChanges {
        changed_files: 1,
        commits: 2,
    });
    let fixture = Fixture::new(git);
    fixture.call(ENTER, json!({"name": "w"})).await.unwrap();
    assert_eq!(
        message(fixture.validate(EXIT, json!({"action": "remove"})).await),
        (
            "Worktree has 1 uncommitted file and 2 commits on worktree-w. Removing will discard this work permanently. Confirm with the user, then re-invoke with discard_changes: true \u{2014} or use action: \"keep\" to preserve the worktree.".to_owned(),
            Some(2)
        )
    );
    assert_eq!(
        fixture
            .validate(EXIT, json!({"action": "remove", "discard_changes": true}))
            .await,
        Validation::Ok
    );
    let text = fixture
        .call(EXIT, json!({"action": "remove", "discard_changes": true}))
        .await
        .unwrap();
    assert_eq!(
        text,
        "Exited and removed worktree at /Users/dev/work/.zcode/worktrees/w. Discarded 2 commits and 1 uncommitted file. Session is now back in /Users/dev/work."
    );
    assert_eq!(
        *fixture
            .env
            .removed
            .lock()
            .unwrap_or_else(PoisonError::into_inner),
        ["/Users/dev/work/.zcode/worktrees/w"]
    );
}

#[tokio::test]
async fn unverifiable_state_blocks_removal() {
    let mut git = Git::new();
    git.changes = None;
    let fixture = Fixture::new(git);
    fixture.call(ENTER, json!({"name": "w"})).await.unwrap();
    let (text, code) = message(fixture.validate(EXIT, json!({"action": "remove"})).await);
    assert!(
        text.starts_with("Could not verify worktree state at /Users/dev/work/.zcode/worktrees/w.")
    );
    assert_eq!(code, Some(3));
}

#[tokio::test]
async fn entering_existing_worktrees() {
    let fixture = Fixture::new(Git::new());
    let context = fixture.context();
    let enter = fixture.tool(ENTER);
    let managed = json!({"path": ".zcode/worktrees/old"});
    assert_eq!(
        enter.check_permissions(&managed, &context).await,
        PermissionDecision::Allow {
            updated_input: None
        }
    );
    assert_eq!(
        enter
            .check_permissions(&json!({"path": "/Users/dev/elsewhere"}), &context)
            .await,
        PermissionDecision::Ask {
            message: texts::ask_enter("/Users/dev/elsewhere"),
            suggestions: Vec::new(),
        }
    );
    let error = fixture
        .call(ENTER, json!({"path": "/Users/dev/nowhere"}))
        .await
        .unwrap_err();
    assert!(
        error.contains(&texts::not_registered("/Users/dev/nowhere", ROOT)),
        "{error}"
    );
    let text = fixture.call(ENTER, managed).await.unwrap();
    assert!(text.starts_with(
        "Entered worktree at /Users/dev/work/.zcode/worktrees/old on branch worktree-old."
    ));
    // 经 path 进入的已有 worktree 不能删。
    let (text, code) = message(fixture.validate(EXIT, json!({"action": "remove"})).await);
    assert_eq!(code, Some(4));
    assert!(text.starts_with(
        "This session is not the owner of the worktree at /Users/dev/work/.zcode/worktrees/old"
    ));
}

/// 修复线 F51c（用户决定 2026-10-22，tools-more.md §11.1）：本工程的 Agent 没有 isolation（agents.md §2），EnterWorktree
/// 的描述不说“子代理因 isolation 固定工作目录”，只留 `(explicit cwd)`。
#[test]
fn enter_worktree_does_not_mention_subagent_isolation() {
    let fixture = Fixture::new(Git::new());
    let prompt = rust_cli_kernel_api::tool::PromptContext {
        tool_names: Vec::new(),
        mode: rust_cli_kernel_api::permission::PermissionMode::Default,
    };
    let description = fixture.tool("EnterWorktree").description(&prompt);
    assert!(!description.contains("subagent isolation"), "{description}");
    assert!(
        description
            .contains("from agents whose working directory was pinned at launch (explicit cwd)."),
        "{description}"
    );
}

/// 工作线 F57：Windows 上 git 列出的是 `C:/…`，解析后的 `path` 是 `C:\…`；以前按字符串比较，带 `path` 总报 not
/// registered，托管位置的判断（`<根>/.zcode/worktrees/` 的字符串前缀）也用错了写法。现在用 kernel-api `env_path` 的键
/// （规范化后不分大小写），进入后的工作目录是本机写法。
#[tokio::test]
async fn windows_worktrees_are_compared_by_path_key() {
    let mut git = Git::new();
    git.windows = true;
    let mut fixture = Fixture::new(git);
    fixture.root = Some(WINDOWS_ROOT);
    let context = fixture.context();
    let enter = fixture.tool(ENTER);
    for path in [
        r".zcode\worktrees\old",
        r"C:\Users\dev\work\.zcode\worktrees\old",
        r"c:\users\dev\work\.zcode\worktrees\OLD\",
    ] {
        assert_eq!(
            enter
                .check_permissions(&json!({"path": path}), &context)
                .await,
            PermissionDecision::Allow {
                updated_input: None
            },
            "{path}"
        );
    }
    // 登记了、但不在托管目录里：询问（路径是本机写法）。
    assert_eq!(
        enter
            .check_permissions(&json!({"path": r"C:\Users\dev\elsewhere"}), &context)
            .await,
        PermissionDecision::Ask {
            message: texts::ask_enter(r"C:\Users\dev\elsewhere"),
            suggestions: Vec::new(),
        }
    );
    let error = fixture
        .call(ENTER, json!({"path": r"C:\Users\dev\nowhere"}))
        .await
        .unwrap_err();
    assert!(
        error.contains(&texts::not_registered(
            r"C:\Users\dev\nowhere",
            WINDOWS_ROOT
        )),
        "{error}"
    );
    let text = fixture
        .call(
            ENTER,
            json!({"path": r"c:\users\dev\work\.zcode\worktrees\old"}),
        )
        .await
        .unwrap();
    assert!(
        text.starts_with(
            r"Entered worktree at C:\Users\dev\work\.zcode\worktrees\old on branch worktree-old."
        ),
        "{text}"
    );
    assert_eq!(
        fixture.switches(),
        [r"C:\Users\dev\work\.zcode\worktrees\old"]
    );
}
