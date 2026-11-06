//! `/resume` 的会话列表上旧界面才有的几步的界面文字（docs/specs/rust-cli/tui-manage.md §10.1）：中文与英文。以前也是 ←
//! 项目页与 `/resume` 整页的文字，那两页 2026-10-31 删掉。
use super::i18n::Lang;

/// ⌃b 在别的文件夹新建会话之后（tui-panels.md §2.4）。
pub fn started_in(lang: Lang, path: &str) -> String {
    match lang {
        Lang::En => format!("Started a new session in {path}."),
        Lang::Zh => format!("已在 {path} 新建会话。"),
    }
}

pub fn rename_failed(lang: Lang, reason: &str) -> String {
    match lang {
        Lang::En => format!("Failed to rename session: {reason}"),
        Lang::Zh => format!("改名失败：{reason}"),
    }
}

pub fn delete_failed(lang: Lang, reason: &str) -> String {
    match lang {
        Lang::En => format!("Failed to delete session: {reason}"),
        Lang::Zh => format!("删除失败：{reason}"),
    }
}

pub fn delete_current(lang: Lang) -> &'static str {
    lang.pick(
        "Use /clear to leave the current session before deleting it.",
        "先用 /clear 离开当前会话再删除它。",
    )
}
