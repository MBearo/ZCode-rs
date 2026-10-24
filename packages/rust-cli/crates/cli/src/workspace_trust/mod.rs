//! 工作区信任（docs/specs/rust-cli/workspace-trust.md §2）：TUI 启动时、启动扩展与建会话
//! 之前判定一次；没有信任就问（[`rust_cli_tui::trust::ask`]），同意后写记录（home 只本次有效），拒绝就退出。
//! `-p` 不经过这里。
//!
//! ```text
//! cwd ──stat .git──▶ git 根 ──读 .git / commondir / gitdir──▶ 规范根（worktree 的主仓库）
//! 候选 = [项目键 = key(规范根 ?? cwd), key(cwd), 上一层 …（仓库里到 git 根为止，否则到根）]
//! store.trusted_workspace(候选) ── 项目键 ──▶ 直接信任
//!                               ── 其他 ────▶ 经父目录信任（项目有授权且不是 home 时补问，§2.5）
//!                               ── 没有 ────▶ 问
//! ```
//!
//! 信任状态的唯一来源是 db 的记录（node-store）；home 的“本次有效”只在这次启动的判定结果里，不另存。

mod git;
mod grants;
mod in_app;
mod key;

use std::path::Path;

use anyhow::Result;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::env_files::PathStyle;
use rust_cli_kernel_api::machine::{EnvInfo, Files};
use rust_cli_kernel_api::store::SessionStore;
use rust_cli_kernel_api::workspace_trust::TrustKey;
use rust_cli_tui::trust::{Grants, TrustAnswer, TrustPrompt};

pub(crate) use self::grants::grants;
pub(crate) use self::in_app::InAppTrust;
use self::key::Key;

/// 判定需要的端口与配置。
pub(crate) struct Inputs<'a> {
    /// 路径事实（home、路径风格）与工作区的 `.git`：都在本机（cli 从注册表的本机取）。
    pub info: &'a dyn EnvInfo,
    pub files: &'a dyn Files,
    pub store: &'a dyn SessionStore,
    pub workspace: &'a WorkspaceRef,
    /// 项目级配置文件里会生效的 allow 规则（`config` 组装时得出）。
    pub grants: Option<Grants>,
}

/// 判定与回答的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// 已信任（或刚同意）：照常启动。
    Continue,
    /// 后备询问里拒绝了项目授权：照常启动，去掉项目级的 allow 规则（§2.5）。
    ContinueWithoutGrants,
    /// 拒绝：不建会话、不启动扩展，以这个退出码退出（不打印文字）。
    Exit(u8),
}

/// 这次启动的信任状态。
enum State {
    /// 项目键自己有记录。
    Direct,
    /// 只有 cwd 或它的某个祖先有记录。
    Inherited,
    Untrusted,
}

/// 判定的结果：已信任，或要问（问什么、同意后写哪个键、是不是 home）。
pub(crate) enum Decision {
    Continue,
    Ask {
        prompt: TrustPrompt,
        key: TrustKey,
        home: bool,
    },
}

/// 判定（§2.1）：读记录失败当作没有信任（记 warn）。启动时与应用内切换项目（§2.8）用同一段。
pub(crate) async fn decide(inputs: Inputs<'_>) -> Decision {
    let facts = inputs.info.path_facts(inputs.workspace).await.ok();
    let style = facts.as_ref().map_or(
        if cfg!(windows) {
            PathStyle::Windows
        } else {
            PathStyle::Posix
        },
        |facts| facts.style,
    );
    let root = inputs.workspace.root.as_str();
    let cwd = Key::parse(root, style);
    let home = facts
        .and_then(|facts| facts.home)
        .is_some_and(|home| Key::parse(home.as_str(), style) == cwd);
    let probe = git::Probe {
        files: inputs.files,
        workspace: inputs.workspace,
        style,
    };
    let git_root = probe.git_root(Path::new(root)).await;
    let project = match &git_root {
        Some(git_root) => {
            let canonical = probe.canonical_root(git_root).await;
            Key::parse(&canonical.to_string_lossy(), style)
        }
        None => cwd.clone(),
    };
    let bound = git_root.map(|git_root| Key::parse(&git_root.to_string_lossy(), style));
    let candidates = candidates(&project, &cwd, bound.as_ref());
    let found = match inputs.store.trusted_workspace(&candidates).await {
        Ok(found) => found,
        Err(error) => {
            tracing::warn!(%error, "the workspace trust records could not be read");
            None
        }
    };
    let project_key = project.trust_key();
    let state = match found {
        Some(key) if key == project_key => State::Direct,
        Some(_) => State::Inherited,
        None => State::Untrusted,
    };
    let backstop = match state {
        State::Direct => return Decision::Continue,
        State::Inherited if home || inputs.grants.is_none() => return Decision::Continue,
        State::Inherited => true,
        State::Untrusted => false,
    };
    Decision::Ask {
        prompt: TrustPrompt {
            path: root.to_owned(),
            backstop,
            grants: inputs.grants,
        },
        key: project_key,
        home,
    }
}

/// 同意之后写记录：home 不写（只本次有效）；写失败记 warn，本次仍按已信任继续（不等
/// 保存的结果）。
pub(crate) async fn record(store: &dyn SessionStore, key: &TrustKey, home: bool) {
    if !home && let Err(error) = store.trust_workspace(key).await {
        tracing::warn!(%error, workspace = %key, "the workspace trust could not be saved");
    }
}

/// 判定、必要时经 `ask` 问用户、处理回答。
pub(crate) async fn ensure(
    inputs: Inputs<'_>,
    ask: impl AsyncFnOnce(TrustPrompt) -> Result<TrustAnswer>,
) -> Result<Outcome> {
    let store = inputs.store;
    let Decision::Ask { prompt, key, home } = decide(inputs).await else {
        return Ok(Outcome::Continue);
    };
    Ok(match ask(prompt).await? {
        TrustAnswer::Trusted => {
            record(store, &key, home).await;
            Outcome::Continue
        }
        TrustAnswer::WithoutGrants => Outcome::ContinueWithoutGrants,
        TrustAnswer::Exit(code) => Outcome::Exit(code),
    })
}

/// 项目键在前；然后 cwd 与它的祖先，在仓库里走到 git 根（含）为止，否则走到根。
fn candidates(project: &Key, cwd: &Key, bound: Option<&Key>) -> Vec<TrustKey> {
    let mut keys = vec![project.trust_key()];
    let mut current = Some(cwd.clone());
    while let Some(key) = current {
        let trust = key.trust_key();
        if !keys.contains(&trust) {
            keys.push(trust);
        }
        if bound == Some(&key) {
            break;
        }
        current = key.parent();
    }
    keys
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
