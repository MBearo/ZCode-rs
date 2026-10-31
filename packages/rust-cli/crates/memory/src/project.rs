//! 自动记忆目录按哪个项目根算（git 根，不在仓库里时是项目根，
//! docs/specs/rust-cli/memory.md §2）：从工作区根往上找 `.git`，经执行环境的 `stat`。

use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::env_files::GIT_DIR;
use rust_cli_kernel_api::env_files::PathStyle;
use rust_cli_kernel_api::env_git::has_git_marker;
use rust_cli_kernel_api::machine::Files;

/// 最近一个含 `.git`（目录或文件）的祖先（从工作区根往上，含根）；都没有时是工作区根。执行环境给不出
/// stat 时也用工作区根。路径规则是 kernel-api `env_path`，风格按工作区根的写法推断（R1-9：以前按“含不含
/// `\`”猜风格、自己找上一层）。
///
/// 缺陷修复（F58）：以前 `.git` 是任何种类都算；git 根只认目录与文件的 `.git`，FIFO 等别的种类要继续往上。
/// 判定改用 kernel-api 的 `has_git_marker`（唯一定义）。
pub async fn project_root(files: &dyn Files, workspace: &WorkspaceRef) -> EnvPath {
    let style = PathStyle::infer(workspace.root.as_str());
    let mut current = Some(style.normalize(workspace.root.as_str()));
    while let Some(dir) = current {
        if has_git_marker(files, workspace, &style.join(&dir, &[GIT_DIR])).await {
            return EnvPath::new(dir);
        }
        current = style.parent(&dir);
    }
    workspace.root.clone()
}

#[cfg(test)]
#[path = "project_tests.rs"]
mod tests;
