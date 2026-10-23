//! 互操作里第一段会话附带的本机设置：“总是允许”的项目规则集（docs/specs/rust-cli/shell-permissions.md
//! §5）、项目的持久模式（config.md §2.2）与生成的会话标题（titles.md §4）；`rules` 命令读回规则集，
//! `mode` / `mode-set` 读写持久模式，给 Node 一侧比对。
use rust_cli_kernel_api::permission::{PermissionRule, RuleBehavior};

use super::*;

pub(super) const GENERATED_TITLE: &str = "Interop session title";

pub(super) async fn write(store: &NodeStore, session: &SessionId) -> Result<()> {
    let remember = Fact::PermissionRulesAdded {
        behavior: RuleBehavior::Allow,
        rules: vec![
            PermissionRule {
                tool: "Bash".into(),
                content: Some("git status:*".into()),
            },
            PermissionRule {
                tool: "Read".into(),
                content: None,
            },
        ],
    };
    store.commit(session, batch(None, vec![remember])).await?;
    // 用户切换到 acceptEdits 时写的项目持久模式（Node 读出 `edit`）。
    let mode = Fact::ProjectModeSet {
        mode: PermissionMode::AcceptEdits,
    };
    store.commit(session, batch(None, vec![mode])).await?;
    let title = Fact::TitleGenerated {
        title: GENERATED_TITLE.into(),
        message: None,
    };
    store.commit(session, batch(None, vec![title])).await?;
    Ok(())
}

pub(super) async fn rules(dir: &str, workspace: &str) -> Result<()> {
    let workspace = WorkspaceRef {
        environment: EnvironmentId::new(LOCAL_ENVIRONMENT),
        root: EnvPath::new(workspace),
    };
    print(&serde_json::to_value(
        store(dir)?.project_rules(&workspace).await?,
    )?)
}

/// `rules-change <dir> <workspace> <ProjectRulesChange 的 JSON>`：`/permissions` 的加与删（tui-manage.md §1.1），
/// 打印落库后的规则集。
pub(super) async fn rules_change(dir: &str, workspace: &str, change: &str) -> Result<()> {
    let workspace = WorkspaceRef {
        environment: EnvironmentId::new(LOCAL_ENVIRONMENT),
        root: EnvPath::new(workspace),
    };
    let change: rust_cli_kernel_api::store::ProjectRulesChange = serde_json::from_str(change)?;
    print(&serde_json::to_value(
        store(dir)?.change_project_rules(&workspace, change).await?,
    )?)
}

/// `mode <dir> <workspace>`：读项目的持久模式（本工程的模式名或 null）；
/// `mode-set <dir> <session> <模式名>`：按会话所在项目写；
/// `mode-exec <dir> <session> <模式名> <true|false>`：写会话的执行状态（权限模式 × plan，修复线 F20）。
pub(super) async fn mode(command: &str, args: &[String]) -> Result<()> {
    match (command, args) {
        ("mode", [dir, workspace]) => {
            let workspace = WorkspaceRef {
                environment: EnvironmentId::new(LOCAL_ENVIRONMENT),
                root: EnvPath::new(workspace.as_str()),
            };
            print(&serde_json::to_value(
                store(dir)?.project_mode(&workspace).await?,
            )?)
        }
        ("mode-set", [dir, session, mode]) => {
            let mode: PermissionMode = serde_json::from_value(json!(mode))?;
            let fact = Fact::ProjectModeSet { mode };
            store(dir)?
                .commit(&SessionId::new(session.as_str()), batch(None, vec![fact]))
                .await?;
            print(&json!({"ok": true}))
        }
        ("mode-exec", [dir, session, mode, plan]) => {
            let mode: PermissionMode = serde_json::from_value(json!(mode))?;
            let state = rust_cli_kernel_api::permission::ExecutionState {
                mode,
                plan: plan == "true",
            };
            let fact = Fact::ExecutionChanged { state };
            store(dir)?
                .commit(&SessionId::new(session.as_str()), batch(None, vec![fact]))
                .await?;
            print(&json!({"ok": true}))
        }
        _ => bail!(
            "usage: node_interop mode <dir> <workspace> | mode-set <dir> <session> <mode> | mode-exec <dir> <session> <mode> <plan>"
        ),
    }
}
