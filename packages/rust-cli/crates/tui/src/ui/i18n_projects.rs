//! ← 项目页与 `/resume` 页的界面文字（修复线 F29，docs/specs/rust-cli/tui-panels.md §2、tui-manage.md §10）：中文与
//! 英文。英文取 Codex 的原文（项目页的组织照 Codex 命令中心，`/resume` 页照 Codex 的 resume 选择器）。
use super::i18n::Lang;

fn pick(lang: Lang, en: &'static str, zh: &'static str) -> &'static str {
    match lang {
        Lang::En => en,
        Lang::Zh => zh,
    }
}

/// 应用内的 ctrl 键（Codex 在提示里写 `⌃`）。
pub const CTRL: &str = "⌃";

pub fn projects_title(lang: Lang) -> &'static str {
    pick(lang, "Projects", "项目")
}

/// footer 在输入框为空时的提示：`← for projects · ? for shortcuts`；`compact` 时只留前一半。
pub fn empty_hint(lang: Lang, compact: bool) -> &'static str {
    match (lang, compact) {
        (Lang::En, false) => "{←} for projects · {?} for shortcuts",
        (Lang::En, true) => "{←} for projects",
        (Lang::Zh, false) => "{←} 查看项目 · {?} 查看快捷键",
        (Lang::Zh, true) => "{←} 查看项目",
    }
}

/// footer 里可以点击的 `← for projects`。
pub fn for_projects(lang: Lang) -> &'static str {
    pick(lang, "{←} for projects", "{←} 查看项目")
}

pub fn details(lang: Lang) -> &'static str {
    pick(lang, "Session details", "会话详情")
}

pub fn project(lang: Lang) -> &'static str {
    pick(lang, "Project", "项目")
}

pub fn branch(lang: Lang) -> &'static str {
    pick(lang, "Branch", "分支")
}

pub fn prompt(lang: Lang) -> &'static str {
    pick(lang, "Prompt", "首条输入")
}

pub fn search_prefix(lang: Lang) -> &'static str {
    pick(lang, "Search › ", "搜索 › ")
}

pub fn rename_prefix(lang: Lang) -> &'static str {
    pick(lang, "Rename › ", "改名 › ")
}

/// 项目页最后一行的提示（`{键}` 模板）：平时、搜索时、改名时。
pub fn projects_hint(lang: Lang, searching: bool, renaming: bool) -> &'static str {
    match (lang, searching, renaming) {
        (Lang::En, _, true) => "{enter} rename · {esc} cancel",
        (Lang::En, true, _) => "{enter} open · {esc} cancel",
        (Lang::En, ..) => {
            "{↑/↓} move · {enter} open · {n} new · {/} search · {r} rename · {o} resume · {esc} back"
        }
        (Lang::Zh, _, true) => "{enter} 改名 · {esc} 取消",
        (Lang::Zh, true, _) => "{enter} 打开 · {esc} 取消",
        (Lang::Zh, ..) => {
            "{↑/↓} 移动 · {enter} 打开 · {n} 新建 · {/} 搜索 · {r} 改名 · {o} 恢复 · {esc} 返回"
        }
    }
}

/// 项目页取到上限时末尾的说明。
pub fn projects_more(lang: Lang, limit: usize) -> String {
    match lang {
        Lang::En => format!("Showing the {limit} most recently updated sessions"),
        Lang::Zh => format!("只显示最近更新的 {limit} 个会话"),
    }
}

/// 选中当前会话（Codex `ignore_same_thread_resume`）。
pub fn already_viewing(lang: Lang, title: &str) -> String {
    match lang {
        Lang::En => format!("Already viewing {title}."),
        Lang::Zh => format!("已经在看 {title}。"),
    }
}

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

pub fn resume_title(lang: Lang) -> &'static str {
    pick(lang, "Resume a previous session", "恢复以前的会话")
}

pub fn filter_label(lang: Lang) -> &'static str {
    pick(lang, "Filter:", "筛选：")
}

pub fn sort_label(lang: Lang) -> &'static str {
    pick(lang, "Sort:", "排序：")
}

/// Filter 的两个值（当前目录、全部）。
pub fn filter_value(lang: Lang, all: bool) -> &'static str {
    match all {
        false => pick(lang, "Cwd", "当前目录"),
        true => pick(lang, "All", "全部"),
    }
}

/// Sort 的两个值（更新时间、创建时间）。
pub fn sort_value(lang: Lang, created: bool) -> &'static str {
    match created {
        false => pick(lang, "Updated", "更新时间"),
        true => pick(lang, "Created", "创建时间"),
    }
}

pub fn type_to_search(lang: Lang) -> &'static str {
    pick(lang, "Type to search", "键入以搜索")
}

pub fn search_query(lang: Lang, query: &str) -> String {
    match lang {
        Lang::En => format!("Search: {query}"),
        Lang::Zh => format!("搜索：{query}"),
    }
}

pub fn no_message(lang: Lang) -> &'static str {
    pick(lang, "(no message yet)", "（还没有消息）")
}

pub fn more_above(lang: Lang) -> &'static str {
    pick(lang, "↑ more", "↑ 更多")
}

pub fn more_below(lang: Lang) -> &'static str {
    pick(lang, "↓ more", "↓ 更多")
}

pub fn no_results(lang: Lang) -> &'static str {
    pick(lang, "No results for your search", "没有匹配的会话")
}

pub fn loading_sessions(lang: Lang) -> &'static str {
    pick(lang, "Loading sessions…", "正在加载会话…")
}

pub fn no_sessions_yet(lang: Lang) -> &'static str {
    pick(lang, "No sessions yet", "还没有会话")
}

pub fn resume_more(lang: Lang, limit: usize) -> String {
    match lang {
        Lang::En => format!("Showing the {limit} most recent sessions"),
        Lang::Zh => format!("只显示最近的 {limit} 个会话"),
    }
}

pub fn no_branch(lang: Lang) -> &'static str {
    pick(lang, "no branch", "没有分支")
}

/// 展开的预览里的一个标签。
#[derive(Debug, Clone, Copy)]
pub enum PreviewLabel {
    Session,
    Created,
    Updated,
    Directory,
    Branch,
    Conversation,
}

/// 展开的预览里的标签（Codex 的 10 宽标签）。
pub fn preview_label(lang: Lang, which: PreviewLabel) -> &'static str {
    let which = which as usize;
    const EN: [&str; 6] = [
        "Session:",
        "Created:",
        "Updated:",
        "Directory:",
        "Branch:",
        "Conversation:",
    ];
    const ZH: [&str; 6] = ["会话：", "创建：", "更新：", "目录：", "分支：", "对话："];
    match lang {
        Lang::En => EN[which],
        Lang::Zh => ZH[which],
    }
}

pub fn preview_loading(lang: Lang) -> &'static str {
    pick(lang, "Loading recent transcript...", "正在加载最近的对话…")
}

pub fn preview_failed(lang: Lang) -> &'static str {
    pick(
        lang,
        "Could not load transcript preview",
        "无法加载对话预览",
    )
}

pub fn preview_empty(lang: Lang) -> &'static str {
    pick(lang, "No transcript preview available", "没有可预览的对话")
}

pub fn loading_transcript(lang: Lang) -> &'static str {
    pick(lang, "Loading transcript…", "正在加载对话…")
}

pub fn transcript_hint(lang: Lang) -> &'static str {
    pick(lang, "{↑/↓} scroll · {q} close", "{↑/↓} 滚动 · {q} 关闭")
}

pub fn delete_title(lang: Lang, name: &str) -> String {
    match lang {
        Lang::En => format!("Permanently delete “{name}”?"),
        Lang::Zh => format!("永久删除“{name}”？"),
    }
}

pub fn delete_note(lang: Lang) -> &'static str {
    pick(lang, "This cannot be undone.", "此操作无法撤销。")
}

/// 删除确认的两项：取消、删除。
pub fn delete_choice(lang: Lang, delete: bool) -> &'static str {
    match delete {
        false => pick(lang, "Cancel", "取消"),
        true => pick(lang, "Permanently delete session", "永久删除会话"),
    }
}

pub fn select_hint(lang: Lang) -> &'static str {
    pick(
        lang,
        "{enter} select · {esc} cancel",
        "{enter} 选择 · {esc} 取消",
    )
}

pub fn delete_current(lang: Lang) -> &'static str {
    pick(
        lang,
        "Use /clear to leave the current session before deleting it.",
        "先用 /clear 离开当前会话再删除它。",
    )
}

pub fn cwd_title(lang: Lang) -> &'static str {
    pick(lang, "Working directory · resume", "工作目录 · 恢复")
}

pub fn cwd_note(lang: Lang) -> [&'static str; 2] {
    match lang {
        Lang::En => [
            "Session = latest cwd recorded in the resumed session",
            "Current = your current working directory",
        ],
        Lang::Zh => [
            "会话 = 要恢复的会话记录的工作目录",
            "当前 = 你现在的工作目录",
        ],
    }
}

/// 目录选择的两项（会话目录、当前目录）。
pub fn cwd_choice(lang: Lang, current: bool, path: &str) -> String {
    match (lang, current) {
        (Lang::En, false) => format!("Use session directory ({path})"),
        (Lang::En, true) => format!("Use current directory ({path})"),
        (Lang::Zh, false) => format!("使用会话目录（{path}）"),
        (Lang::Zh, true) => format!("使用当前目录（{path}）"),
    }
}

pub fn cwd_hint(lang: Lang) -> &'static str {
    pick(
        lang,
        "{enter} continue · {esc} use session · {⌃c} exit",
        "{enter} 继续 · {esc} 用会话目录 · {⌃c} 退出",
    )
}

/// `/resume` 页 footer 的一条提示：（宽版说明，窄版说明）。`which` 见 `resume_draw::Hint`。
pub fn footer_label(lang: Lang, which: &str) -> (&'static str, &'static str) {
    let en = |wide, compact| (wide, compact);
    match (lang, which) {
        (Lang::En, "resume") => en("resume", "resume"),
        (Lang::En, "rename") => en("rename", "rename"),
        (Lang::En, "delete") => en("delete", "delete"),
        (Lang::En, "exit") => en("exit", "exit"),
        (Lang::En, "clear") => en("clear search", "clear"),
        (Lang::En, "new") => en("start new", "new"),
        (Lang::En, "quit") => en("quit", "quit"),
        (Lang::En, "focus") => en("focus sort/filter", "focus"),
        (Lang::En, "option") => en("change option", "option"),
        (Lang::En, "comfortable") => en("comfortable view", "comfy"),
        (Lang::En, "dense") => en("dense view", "dense"),
        (Lang::En, "transcript") => en("transcript", "preview"),
        (Lang::En, "expand") => en("expand", "exp"),
        (Lang::En, "browse") => en("browse", "browse"),
        (Lang::Zh, "resume") => en("恢复", "恢复"),
        (Lang::Zh, "rename") => en("改名", "改名"),
        (Lang::Zh, "delete") => en("删除", "删除"),
        (Lang::Zh, "exit") => en("退出", "退出"),
        (Lang::Zh, "clear") => en("清除搜索", "清除"),
        (Lang::Zh, "new") => en("新会话", "新会话"),
        (Lang::Zh, "quit") => en("退出 zcode", "退出"),
        (Lang::Zh, "focus") => en("切换焦点", "焦点"),
        (Lang::Zh, "option") => en("切换选项", "选项"),
        (Lang::Zh, "comfortable") => en("宽松视图", "宽松"),
        (Lang::Zh, "dense") => en("紧凑视图", "紧凑"),
        (Lang::Zh, "transcript") => en("对话", "对话"),
        (Lang::Zh, "expand") => en("展开", "展开"),
        (Lang::Zh, "browse") => en("浏览", "浏览"),
        _ => ("", ""),
    }
}
