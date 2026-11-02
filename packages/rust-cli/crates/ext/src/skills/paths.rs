//! `paths` 条件 skill（docs/specs/rust-cli/ext.md §2.6）：历史里成功的 Read / Write / Edit 触及的文件命中模式后激活，
//! 之后一直可用。
//!
//! 激活不记在进程状态里，而是从历史推导（与 nested memory 相同，context.md §11.6），
//! resume 后结果相同。

use rust_cli_base::glob::Globs;
use rust_cli_kernel_api::env_files::PathStyle;
use rust_cli_kernel_api::history::touched_files;
use rust_cli_kernel_api::message::Message;

use super::Skill;

/// 历史里成功的 Read / Write / Edit 触及的文件，换成相对 cwd 的 `/` 路径；越出 cwd 的不算。触发的工具与“成功”
/// 的判断只在 kernel-api 的 `history::touched_files`（F132 的 T9：以前这里与 context 的 nested memory 各写一份）。
pub(crate) fn touched(history: &[Message], cwd: &str) -> Vec<String> {
    touched_files(history)
        .into_iter()
        .filter_map(|path| relative_to(path, cwd))
        .collect()
}

/// 相对 cwd 的 `/` 路径（`path.relative(cwd, file)`）；在 cwd 外面或就是 cwd 时为 None。
/// 路径规则是 kernel-api `env_path`（R1-9：以前按“含不含 `\`”猜风格、只做前缀比较）。
fn relative_to(path: &str, cwd: &str) -> Option<String> {
    let relative = PathStyle::infer(cwd)
        .relative(cwd, path)?
        .replace('\\', "/");
    (!relative.is_empty() && !relative.starts_with("..")).then_some(relative)
}

/// 无条件的 skill 总是可用；条件 skill 在触及的文件命中它的模式之后可用。
pub(crate) fn active(skill: &Skill, touched: &[String]) -> bool {
    let Some(patterns) = &skill.paths else {
        return true;
    };
    let globs = Globs::new(patterns);
    touched.iter().any(|path| globs.matches(path))
}

#[cfg(test)]
#[path = "paths_tests.rs"]
mod tests;
