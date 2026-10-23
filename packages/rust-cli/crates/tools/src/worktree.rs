//! EnterWorktree 与 worktree 会话的记录（docs/specs/rust-cli/tools-more.md §11）。ExitWorktree 在
//! [`crate::worktree_exit`]。
//!
//! git 操作经执行环境（`Environment::worktree_*`）；会话的工作目录只有会话 actor 一个 owner，工具经
//! `Interactions::set_workspace` 请求修改（§11.4）。worktree 会话的记录按
//! 顶层会话放在 [`WorktreeBoard`]，只在改目录成功后记录或清除。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use rust_cli_base::names::{DOT_DIR, WORKTREES_DIR};
use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::env_files::PathStyle;
use rust_cli_kernel_api::env_worktree::{WorktreeCreate, WorktreeCreated};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::permission::PermissionDecision;
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext, Validation,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::common::{Definition, definition, env_failure, failed, parse, resolve, style, text};
use crate::session_slug::session_slug;
use crate::worktree_exit::ExitWorktreeTool;
use crate::worktree_texts as texts;

pub const ENTER: &str = "EnterWorktree";

/// 一个顶层会话当前所在的 worktree。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorktreeSession {
    pub(crate) original: EnvPath,
    pub(crate) path: EnvPath,
    pub(crate) branch: Option<String>,
    /// 创建时的基点：ExitWorktree 据此数新提交。
    pub(crate) head_commit: Option<String>,
    /// 经 `path` 进入的已有 worktree：ExitWorktree 不删除它。
    pub(crate) entered_existing: bool,
}

#[derive(Default)]
pub struct WorktreeBoard(Mutex<HashMap<SessionId, WorktreeSession>>);

impl WorktreeBoard {
    pub(crate) fn sessions(&self) -> MutexGuard<'_, HashMap<SessionId, WorktreeSession>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn get(&self, owner: &SessionId) -> Option<WorktreeSession> {
        self.sessions().get(owner).cloned()
    }

    /// 会话关闭（session-lifecycle.md §3.3）：丢掉它的记录。工作目录不落库，再打开的会话回到创建时的目录
    /// （tools-more.md D23），留着记录会让它以为还在 worktree 里。
    pub fn forget(&self, owner: &SessionId) {
        self.sessions().remove(owner);
    }
}

/// 两个工具共用一份记录。
pub fn worktree_tools(board: &Arc<WorktreeBoard>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(EnterWorktreeTool {
            definition: definition(ENTER),
            board: board.clone(),
        }),
        Arc::new(ExitWorktreeTool::new(board)),
    ]
}

pub(crate) async fn owner(context: &ToolUseContext) -> SessionId {
    context.interactions.owner_session(&context.session).await
}

/// 名字：总长不超过 64；按 `/` 分段，每段非空、只含字母数字与 `._-`，不能是 `.` / `..` / `.git`。
fn name_error(name: &str) -> Option<String> {
    if name.len() > texts::MAX_NAME {
        return Some(texts::too_long(name.len()));
    }
    for segment in name.split('/') {
        if segment == "." || segment == ".." {
            return Some(texts::invalid_name(
                name,
                "must not contain \".\" or \"..\" path segments",
            ));
        }
        if segment.to_lowercase().trim_end_matches('.') == ".git" {
            let reason = format!("\"{segment}\" is a reserved git directory name");
            return Some(texts::invalid_name(name, &reason));
        }
        let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-');
        if segment.is_empty() || !segment.chars().all(allowed) {
            return Some(texts::invalid_name(
                name,
                "each \"/\"-separated segment must be non-empty and contain only letters, digits, dots, underscores, and dashes",
            ));
        }
    }
    None
}

/// `path` 严格在仓库 `root` 的 `.zcode/worktrees/` 里。
fn managed(style: PathStyle, path: &str, root: &str) -> bool {
    let managed = style.join(root, &[DOT_DIR, WORKTREES_DIR]);
    style.is_within(path, &managed) && !style.same(path, &managed)
}

pub(crate) fn invalid(message: String, code: Option<i32>) -> Validation {
    Validation::Invalid {
        message,
        error_code: code,
    }
}

pub(crate) async fn switch(context: &ToolUseContext, root: EnvPath) -> Result<(), ToolError> {
    context
        .interactions
        .set_workspace(&context.session, root)
        .await
        .map_err(|error| failed(error.to_string()))
}

#[derive(Deserialize)]
struct EnterInput {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    path: Option<String>,
}

pub struct EnterWorktreeTool {
    definition: Definition,
    board: Arc<WorktreeBoard>,
}

impl EnterWorktreeTool {
    /// `path` 是当前仓库 `git worktree list` 里的一项时返回它、所在的主仓库根与分支（都是规范化的本机写法）。
    ///
    /// 缺陷修复（工作线 F57）：以前按字符串比较（只去掉末尾的 `/`），Windows 上 git 列出 `C:/…`、解析后的 `path` 是
    /// `C:\…`，带 `path` 总报 not registered。现在用 kernel-api `env_path` 的键比较（规范化，Windows 上不分大小写），
    /// 近似比较两边的 realpath（kernel 不做 realpath）。
    async fn registered(
        &self,
        context: &ToolUseContext,
        raw: &str,
    ) -> Result<(String, String, Option<String>), ToolError> {
        let style = style(context);
        let wanted = resolve(context, context.workspace.root.as_str(), raw).await?;
        let listed = context
            .env
            .worktree_list(context.workspace.root.clone())
            .await
            .map_err(env_failure)?;
        let root = style.normalize(
            listed
                .root
                .as_ref()
                .unwrap_or(&context.workspace.root)
                .as_str(),
        );
        let entry = listed
            .entries
            .into_iter()
            .find(|entry| style.same(entry.path.as_str(), &wanted));
        match entry {
            Some(entry) => Ok((style.normalize(entry.path.as_str()), root, entry.branch)),
            None => Err(failed(texts::not_registered(&wanted, &root))),
        }
    }
}

#[async_trait]
impl Tool for EnterWorktreeTool {
    fn name(&self) -> &str {
        ENTER
    }

    fn input_schema(&self) -> Value {
        self.definition.schema.clone()
    }

    fn description(&self, _context: &PromptContext) -> String {
        self.definition.description.clone()
    }

    fn should_defer(&self) -> bool {
        true
    }

    fn search_hint(&self) -> Option<&str> {
        Some("create an isolated git worktree and switch into it")
    }

    fn max_result_size_chars(&self) -> Option<usize> {
        Some(100_000)
    }

    async fn validate_input(&self, input: &Value, context: &ToolUseContext) -> Validation {
        let Ok(input) = parse::<EnterInput>(input) else {
            return Validation::Ok;
        };
        if input.name.is_some() && input.path.is_some() {
            return invalid(texts::BOTH_GIVEN.to_owned(), None);
        }
        if let Some(message) = input.name.as_deref().and_then(name_error) {
            return invalid(message, None);
        }
        if input.path.is_none() && self.board.get(&owner(context).await).is_some() {
            return invalid(texts::ALREADY_IN.to_owned(), Some(2));
        }
        Validation::Ok
    }

    /// 新建：放行。进入已有 worktree：在本仓库 `.zcode/worktrees/` 下且已登记时放行，否则询问。
    async fn check_permissions(
        &self,
        input: &Value,
        context: &ToolUseContext,
    ) -> PermissionDecision {
        let allow = PermissionDecision::Allow {
            updated_input: None,
        };
        let Some(raw) = input.get("path").and_then(Value::as_str) else {
            return allow;
        };
        let style = style(context);
        match self.registered(context, raw).await {
            // 严格在本仓库的 `.zcode/worktrees/` 里（F57：按 `env_path` 的键，以前是 `<根>/.zcode/worktrees/` 的字符串前缀，
            // Windows 上写法对不上）。
            Ok((path, root, _)) if managed(style, &path, &root) => allow,
            Ok((path, ..)) => PermissionDecision::Ask {
                message: texts::ask_enter(&path),
                suggestions: Vec::new(),
            },
            Err(_) => match resolve(context, context.workspace.root.as_str(), raw).await {
                Ok(path) => PermissionDecision::Ask {
                    message: texts::ask_enter(&path),
                    suggestions: Vec::new(),
                },
                Err(_) => allow,
            },
        }
    }

    async fn call(
        &self,
        input: Value,
        context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let input: EnterInput = parse(&input)?;
        let owner = owner(&context).await;
        let current = self.board.get(&owner);
        if current.is_some() && input.path.is_none() {
            return Err(failed("Already in a worktree session".to_owned()));
        }
        // 切换到另一个 worktree 时，原目录仍是进入第一个 worktree 之前的目录。
        let original = current.map_or_else(
            || context.workspace.root.clone(),
            |session| session.original,
        );
        let (session, verb, note) = match &input.path {
            Some(raw) => {
                let (path, _, branch) = self.registered(&context, raw).await?;
                let session = WorktreeSession {
                    original,
                    path: EnvPath::new(path),
                    branch,
                    head_commit: None,
                    entered_existing: true,
                };
                (session, "Entered", "")
            }
            None => {
                // 缺省名是顶层会话的词组（tools-more.md §11.6），原来用会话 ID。
                let name = input.name.clone().unwrap_or_else(|| session_slug(&owner));
                let request = WorktreeCreate {
                    cwd: context.workspace.root.clone(),
                    name,
                };
                let created = context
                    .env
                    .worktree_create(context.operation.clone(), request)
                    .await
                    .map_err(env_failure)?;
                match created {
                    WorktreeCreated::Ready {
                        path,
                        branch,
                        head_commit,
                        existed,
                    } => {
                        let session = WorktreeSession {
                            original,
                            path,
                            branch: Some(branch),
                            head_commit,
                            entered_existing: false,
                        };
                        let (verb, note) = if existed {
                            ("Resumed", texts::RESUMED_NOTE)
                        } else {
                            ("Created", "")
                        };
                        (session, verb, note)
                    }
                    WorktreeCreated::NotARepository => {
                        return Err(failed(texts::NOT_A_REPO.to_owned()));
                    }
                    WorktreeCreated::Failed { message } => return Err(failed(message)),
                }
            }
        };
        switch(&context, session.path.clone()).await?;
        let message = texts::entered(verb, session.path.as_str(), session.branch.as_deref(), note);
        let display =
            json!({"worktreePath": session.path.as_str(), "worktreeBranch": session.branch});
        self.board.sessions().insert(owner, session);
        let mut output = text(message);
        output.display = display;
        Ok(output)
    }
}

#[cfg(test)]
#[path = "worktree_tests.rs"]
mod tests;
