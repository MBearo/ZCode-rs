//! 计划文件的位置（docs/specs/rust-cli/tools-more.md §2.2）。
//!
//! 唯一来源：ExitPlanMode 读计划、context 的计划模式 attachment、policy 的计划文件放行都从这里取，
//! 不各算各的。纯字符串运算，不做 IO：路径风格（与旧布局的 home）由调用方经 `EnvInfo::path_facts`
//! 取，工作区根取调用方手里的 `WorkspaceRef.root`。

use rust_cli_base::names::DOT_DIR;
use serde::{Deserialize, Serialize};

use crate::env::EnvPath;
use crate::env_files::{PathFacts, PathStyle};
use crate::ids::{AgentId, SessionId};
use crate::permission::PermissionMode;

/// `plan_mode_reentry` attachment 的来源（context 产生、会话 actor 提交时清标志，同一个名字）。
pub const REENTRY_SOURCE: &str = "plan_mode_reentry";
/// `plan_mode_exit` attachment 的来源。
pub const EXIT_SOURCE: &str = "plan_mode_exit";

/// 计划模式的两个会话标志（离开过 plan、要发退出提醒；
/// docs/specs/rust-cli/context.md §13.4）。会话 actor 持有，在改模式的
/// 同一处更新，不持久化；经 `ContextRequest.plan_flags` 交给 context。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanModeFlags {
    /// 离开过 plan 模式，还没发 `plan_mode_reentry`。
    #[serde(default)]
    pub has_exited: bool,
    /// 刚离开 plan 模式，还没发 `plan_mode_exit`。
    #[serde(default)]
    pub needs_exit_attachment: bool,
}

impl PlanModeFlags {
    /// 模式从 `from` 变成 `to` 之后的标志（
    /// 进入 plan 清掉待发的退出提醒；离开 plan 两个标志都置真；模式没变时不动）。
    pub fn after_transition(self, from: PermissionMode, to: PermissionMode) -> Self {
        let plan = PermissionMode::Plan;
        match (from == plan, to == plan) {
            (false, true) => Self {
                needs_exit_attachment: false,
                ..self
            },
            (true, false) => Self {
                has_exited: true,
                needs_exit_attachment: true,
            },
            _ => self,
        }
    }

    /// 提交了来源为 `source` 的 attachment 之后的标志（产生 `plan_mode_reentry` / `plan_mode_exit`
    /// 时清对应的标志）。
    pub fn after_commit(self, source: &str) -> Self {
        match source {
            REENTRY_SOURCE => Self {
                has_exited: false,
                ..self
            },
            EXIT_SOURCE => Self {
                needs_exit_attachment: false,
                ..self
            },
            _ => self,
        }
    }
}

fn separator(style: PathStyle) -> char {
    match style {
        PathStyle::Posix => '/',
        PathStyle::Windows => '\\',
    }
}

/// 计划文件放在哪里。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    /// L11c 的旧布局：`<home>/.zcode/plans/<会话>.md`，子代理 `<会话>-agent-<agent>.md`。
    Home,
    /// Node 的位置（2026-10-18）：`<工作区>/.zcode/plans/plan-<会话>.md`。
    Workspace,
}

/// 本会话的计划文件。
///
/// 现在用 [`PlanFiles::in_workspace`]：`<工作区>/.zcode/plans/plan-<会话>.md`（Node
/// `resolveApprovedPlanFilePath`）。[`PlanFiles::new`] 是 L11c 的旧布局（`<home>/.zcode/plans/<会话>.md`，
/// 文件名不用随机词组），保留不用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanFiles {
    dir: String,
    /// 旧布局是会话 ID 原样；工作区布局是净化后的会话 ID。
    session: String,
    style: PathStyle,
    layout: Layout,
}

/// Node `sanitizePlanFileSessionId`：trim，连续的 `[^A-Za-z0-9._-]` 换成一个 `-`，去掉首尾的 `-`。
fn sanitize(session: &str) -> String {
    let mut out = String::new();
    let mut replaced = false;
    for c in session.trim().chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            out.push(c);
            replaced = false;
        } else if !replaced {
            out.push('-');
            replaced = true;
        }
    }
    out.trim_matches('-').to_owned()
}

impl PlanFiles {
    /// 旧布局（home 下）；home 未知时返回 None。
    pub fn new(facts: &PathFacts, session: &SessionId) -> Option<Self> {
        let home = facts.home.as_ref()?.as_str();
        Some(Self {
            // F58：与执行环境里别的 `.zcode` 一样用 `PathStyle::join`（以前只去末尾的分隔符再拼）。
            dir: facts.style.join(home, &[DOT_DIR, "plans"]),
            session: session.to_string(),
            style: facts.style,
            layout: Layout::Home,
        })
    }

    /// Node 的位置：`<root>/.zcode/plans/plan-<会话>.md`（tools-more.md §2.2）。`root` 是会话的工作目录；
    /// 会话 ID 净化后为空时返回 None（Node 报错）。子代理传子会话的 ID。
    pub fn in_workspace(facts: &PathFacts, root: &EnvPath, session: &SessionId) -> Option<Self> {
        let session = sanitize(session.as_str());
        if session.is_empty() {
            return None;
        }
        Some(Self {
            // F58：与执行环境里别的 `.zcode` 一样用 `PathStyle::join`（Node `path.join`；以前只去末尾的分隔符再拼）。
            dir: facts.style.join(root.as_str(), &[DOT_DIR, "plans"]),
            session,
            style: facts.style,
            layout: Layout::Workspace,
        })
    }

    /// 计划目录。
    pub fn dir(&self) -> &str {
        &self.dir
    }

    /// 文件名：工作区布局只由会话决定（子代理跑在子会话，`agent` 不参与命名）；旧布局主线程
    /// （`agent` 为 None）`<会话>.md`、子代理 `<会话>-agent-<agent>.md`。
    fn file_name(&self, agent: Option<&AgentId>) -> String {
        match (self.layout, agent) {
            (Layout::Workspace, _) => format!("plan-{}.md", self.session),
            (Layout::Home, Some(agent)) => format!("{}-agent-{agent}.md", self.session),
            (Layout::Home, None) => format!("{}.md", self.session),
        }
    }

    /// 主线程（`agent` 为 None）或子代理的计划文件。
    pub fn path(&self, agent: Option<&AgentId>) -> EnvPath {
        let name = self.file_name(agent);
        EnvPath::new(format!("{}{}{name}", self.dir, separator(self.style)))
    }

    /// 是不是本会话的计划文件：父目录是计划目录，并且工作区布局下文件名
    /// 正好是 `plan-<会话>.md`；旧布局下是 `<会话>.md`，或以 `<会话>-agent-` 开头、以 `.md` 结尾。
    /// `path` 是规范化后的绝对路径；Windows 上不分 `/` 与 `\`、不分大小写。
    pub fn contains(&self, path: &str) -> bool {
        let fold = |text: &str| match self.style {
            PathStyle::Posix => text.to_owned(),
            PathStyle::Windows => text.replace('/', "\\").to_lowercase(),
        };
        let path = fold(path);
        let Some((dir, name)) = path.rsplit_once(separator(self.style)) else {
            return false;
        };
        if dir != fold(&self.dir) {
            return false;
        }
        let session = fold(&self.session);
        match self.layout {
            Layout::Workspace => name == fold(&self.file_name(None)),
            Layout::Home => {
                name == format!("{session}.md")
                    || (name.starts_with(&format!("{session}-agent-")) && name.ends_with(".md"))
            }
        }
    }
}

#[cfg(test)]
#[path = "plan_tests.rs"]
mod tests;
