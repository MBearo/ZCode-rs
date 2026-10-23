//! ← 项目页与 `/resume` 页共用的会话行（修复线 F29，docs/specs/rust-cli/tui-panels.md §2、tui-manage.md §10）：
//! 内核快照（`ProjectSession`）到界面行的换算、标题、搜索匹配、时间与路径的写法。只有纯计算。
use rust_cli_kernel_api::message::{ContentBlock, Message};
use rust_cli_kernel_api::panels::{ProjectSession, SessionActivity};
use unicode_width::UnicodeWidthStr;

use super::i18n::Lang;

/// 一个会话（打开页面时的快照）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    pub id: String,
    /// 会话名（会话行的 `title`，去掉首尾空白）。
    pub title: String,
    /// 首条输入（界面原文）。
    pub prompt: String,
    pub root: String,
    pub created: u64,
    pub updated: u64,
    pub branch: Option<String>,
    pub activity: Option<SessionActivity>,
    pub reply: String,
    pub current: bool,
}

impl SessionRow {
    pub fn new(session: &ProjectSession, current: Option<&str>) -> Self {
        let record = &session.record;
        Self {
            id: record.id.as_str().to_owned(),
            title: record.title.trim().to_owned(),
            prompt: record.first_prompt.clone().unwrap_or_default(),
            root: record.root.as_str().to_owned(),
            created: record.created_at_ms,
            updated: record.updated_at_ms,
            branch: session.branch.clone(),
            activity: session.activity,
            reply: session.last_reply.clone().unwrap_or_default(),
            current: current == Some(record.id.as_str()),
        }
    }

    /// 显示的标题：会话名，没有时首条输入的第一行；都没有时为空（调用方写占位文字）。
    pub fn label(&self) -> String {
        let first = |text: &str| text.trim().lines().next().unwrap_or("").trim().to_owned();
        if self.title.is_empty() {
            first(&self.prompt)
        } else {
            first(&self.title)
        }
    }

    /// 不分大小写的子串匹配：项目页比标题、首条输入、路径（Codex 命令中心），`/resume` 页另比会话 ID 与分支
    /// （Codex `matches_query`）。`query` 已转成小写。
    pub fn matches(&self, query: &str, more: bool) -> bool {
        if query.is_empty() {
            return true;
        }
        let hit = |text: &str| text.to_lowercase().contains(query);
        hit(&self.title)
            || hit(&self.prompt)
            || hit(&self.root)
            || (more && (hit(&self.id) || self.branch.as_deref().is_some_and(hit)))
    }
}

/// 一条人类输入放回输入框、显示在预览里的样子（从 runner/lifecycle.rs 挪来，rewind 选择器与 `/resume` 页的预览共用）。
pub fn user_text(message: &Message) -> Option<String> {
    let Message::User(user) = message else {
        return None;
    };
    let texts: Vec<&str> = user
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    let text = texts.join("\n");
    // 用户调用 skill 的输入：放回输入框的是用户输入的样子（`/<名字> <参数>`，ext.md §2.7）；bash 模式的输入
    // 放回成 `!命令`，输入框据此回到 bash 模式（tui-input.md §2.1）。
    if let Some(command) = rust_cli_kernel_api::user_input::display(&text) {
        return Some(format!("!{}", command.trim_start_matches("! ")));
    }
    Some(rust_cli_kernel_api::user_command::display(&text).unwrap_or(text))
}

/// 相对时间（Codex `format_relative_time`）：`now`、`42s ago`、`35m ago`、`2h ago`、`1d ago`。
pub fn relative(lang: Lang, now_ms: u64, at_ms: u64) -> String {
    let seconds = now_ms.saturating_sub(at_ms) / 1000;
    match (lang, seconds) {
        (Lang::En, 0) => "now".to_owned(),
        (Lang::En, 1..60) => format!("{seconds}s ago"),
        (Lang::En, 60..3600) => format!("{}m ago", seconds / 60),
        (Lang::En, 3600..86_400) => format!("{}h ago", seconds / 3600),
        (Lang::En, _) => format!("{}d ago", seconds / 86_400),
        (Lang::Zh, 0) => "刚刚".to_owned(),
        (Lang::Zh, 1..60) => format!("{seconds} 秒前"),
        (Lang::Zh, 60..3600) => format!("{} 分钟前", seconds / 60),
        (Lang::Zh, 3600..86_400) => format!("{} 小时前", seconds / 3600),
        (Lang::Zh, _) => format!("{} 天前", seconds / 86_400),
    }
}

/// 长的相对时间（Codex `format_relative_time_long`）：`1 minute ago`、`5 minutes ago`。
pub fn relative_long(lang: Lang, now_ms: u64, at_ms: u64) -> String {
    let seconds = now_ms.saturating_sub(at_ms) / 1000;
    let (value, unit) = match seconds {
        0 => return relative(lang, now_ms, at_ms),
        1..60 => (seconds, "second"),
        60..3600 => (seconds / 60, "minute"),
        3600..86_400 => (seconds / 3600, "hour"),
        _ => (seconds / 86_400, "day"),
    };
    match lang {
        Lang::En if value == 1 => format!("1 {unit} ago"),
        Lang::En => format!("{value} {unit}s ago"),
        Lang::Zh => relative(lang, now_ms, at_ms),
    }
}

/// 本地时间 `YYYY-MM-DD HH:MM:SS`（`offset_minutes` 是注入时钟给的时区偏移）。
pub fn timestamp(at_ms: u64, offset_minutes: i32) -> String {
    let local = at_ms as i64 + i64::from(offset_minutes) * 60_000;
    let local = u64::try_from(local).unwrap_or(0);
    let seconds = (local / 1000) % 86_400;
    format!(
        "{} {:02}:{:02}:{:02}",
        rust_cli_kernel_api::clock::civil_date(local),
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

/// 中间截断到 `width` 列（路径用，Codex `center_truncate_path`）：前后各留一半，中间是 `…`。
pub fn center_truncate(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_owned();
    }
    if width <= 1 {
        return "…".chars().take(width).collect();
    }
    let keep = width - 1;
    let (front, back) = (keep.div_ceil(2), keep / 2);
    let mut head = String::new();
    for c in text.chars() {
        if (head.clone() + &c.to_string()).width() > front {
            break;
        }
        head.push(c);
    }
    let mut tail: Vec<char> = Vec::new();
    for c in text.chars().rev() {
        let next: String = std::iter::once(c)
            .chain(tail.iter().rev().copied())
            .collect();
        if next.width() > back {
            break;
        }
        tail.insert(0, c);
    }
    format!("{head}…{}", tail.into_iter().collect::<String>())
}

#[cfg(test)]
#[path = "session_rows_tests.rs"]
mod tests;
