//! `/export [filename]`（docs/specs/rust-cli/tui-manage.md §3）：会话文字用界面同一套排版渲染，
//! 复制经 `Screen::copy`，写文件经内核（`Kernel::write_export`）。不带文件名时是 Puffin 的 `/export` 列表（tui-kit.md
//! §4.2「`/export`」，壳的 `shell/export.rs`）：跟着草稿的一层（草稿正好是 `/export ` 时开着），没有头，两行（kit
//! `export_list::EXPORT_ROWS` 的名字与说明）；选了复制就复制，选了存成文件，草稿变成 `/export <缺省文件名>`，在草稿里改名、
//! ⏎ 存。以前是自己的对话框：头一行 `Export conversation  Select export method`，文件名在门道里填，esc 说 `Export cancelled`。
use rust_cli_base::units;

use super::app::{Action, App};
use super::cells::Ctx;
use super::i18n::Lang;
use super::layout;
use super::list::{Choice, Item, ListView, Purpose};
use super::manage::Manage;
use super::transcript::{self, Cache};

/// `/export` 列表开着时的草稿（Puffin `EXPORT_DRAFT`）。
pub const EXPORT_DRAFT: &str = "/export ";

/// 会话的文字：与界面同一套排版（会话头、cell、本地提示，非展开视图），去掉样式与行尾空白。宽度取上一帧
/// transcript 的宽度，没有画过时 80 列。
pub fn conversation_text(app: &App, now_ms: u64) -> (String, Option<String>) {
    let width = match app.pointer.hits.transcript.width {
        0 => layout::DEFAULT_WIDTH,
        width => usize::from(width),
    };
    let label = crate::ui::app::label(app.view.workspace().root.as_str(), app.home.as_deref());
    let ctx = Ctx {
        ink: &app.ink,
        lang: app.lang,
        width,
        expanded: false,
        elapsed: std::time::Duration::ZERO,
        now_ms,
        workspace: app.view.workspace().root.as_str(),
        workspace_label: &label,
        version: &app.version,
        mcp_machines: &app.mcp_machines,
        machines: &app.machines,
        skills: app.composer.mentions.catalog.as_deref().unwrap_or(&[]),
        markdown: None,
    };
    let layout = transcript::lines(app, &mut Cache::default(), &ctx);
    let mut text = String::new();
    for index in 0..layout.len() {
        text.push_str(layout.styled(index).plain().trim_end());
        text.push('\n');
    }
    let first = layout.prompts.first().map(|prompt| prompt.text.clone());
    (text, first)
}

/// 本地时间的 `YYYY-MM-DD-HHMMSS`（`offset_minutes` 是那一刻的 UTC 偏移）。
///
/// 修复（R2-6）：以前按 UTC（tui-manage.md D9）；现在用本地时间。
fn timestamp(now_ms: u64, offset_minutes: i32) -> String {
    let utc = i64::try_from(now_ms).unwrap_or(i64::MAX);
    let offset_ms = i64::from(offset_minutes) * units::MS_PER_MINUTE as i64;
    let local = u64::try_from(utc.saturating_add(offset_ms)).unwrap_or(0);
    let date = rust_cli_kernel_api::clock::civil_date(local);
    let (hours, minutes, seconds) = super::quantities::time_of_day(local);
    format!("{date}-{hours:02}{minutes:02}{seconds:02}")
}

/// 文件名里第一条输入最多取这么多个字符（超出时截到少一个字符再加 `…`）。
const SLUG_SOURCE_CHARS: usize = 50;

/// 缺省文件名：`<时间>-<第一条输入的 slug>.txt`，没有 slug 时 `conversation-<时间>.txt`。
pub fn default_name(first_prompt: Option<&str>, now_ms: u64, offset_minutes: i32) -> String {
    let mut text = first_prompt.unwrap_or_default().trim().to_owned();
    if text.chars().count() > SLUG_SOURCE_CHARS {
        // 留一个字符给 `…`。
        text = text
            .chars()
            .take(SLUG_SOURCE_CHARS - 1)
            .chain(['…'])
            .collect();
    }
    let mut slug = String::new();
    for c in text.to_lowercase().chars() {
        if c.is_whitespace() || c == '-' {
            if !slug.ends_with('-') {
                slug.push('-');
            }
        } else if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
        }
    }
    let slug = slug.trim_matches('-');
    let time = timestamp(now_ms, offset_minutes);
    if slug.is_empty() {
        format!("conversation-{time}.txt")
    } else {
        format!("{time}-{slug}.txt")
    }
}

/// 两种方式的名字与说明（Puffin，kit `export_list::EXPORT_ROWS`；中文是译文）。
fn rows(lang: Lang) -> [(&'static str, &'static str); 2] {
    match lang {
        Lang::En => rust_cli_tui_kit::app::widgets::export_list::EXPORT_ROWS,
        Lang::Zh => [
            ("复制到剪贴板", "把对话复制到剪贴板"),
            ("保存到文件", "把对话存成这个文件夹里的一个文件"),
        ],
    }
}

/// 写好了（Puffin `Exported to <路径>.`，家目录写成 `~`）。以前是 `Conversation exported to: <路径>`。
pub(crate) fn exported(lang: Lang, path: &str) -> String {
    match lang {
        Lang::En => format!("Exported to {path}."),
        Lang::Zh => format!("已导出到 {path}。"),
    }
}

/// 没写成（Puffin `Could not export: <原因>`）。
pub(crate) fn export_failed(lang: Lang, reason: &str) -> String {
    match lang {
        Lang::En => format!("Could not export: {reason}"),
        Lang::Zh => format!("无法导出：{reason}"),
    }
}

impl App {
    /// `/export [filename]`：带文件名时直接写；不带时打开对话框。
    pub(super) fn export_command(&mut self, args: &str) -> Option<Action> {
        if !args.trim().is_empty() {
            return Some(Action::Manage(Manage::Export(args.trim().to_owned())));
        }
        let items = rows(self.lang)
            .iter()
            .zip([Choice::ExportCopy, Choice::ExportFile])
            .map(|((label, description), value)| Item {
                label: (*label).to_owned(),
                description: (*description).to_owned(),
                value,
                ..Item::default()
            })
            .collect();
        self.show_list(ListView::new(
            Purpose::Export,
            String::new(),
            items,
            String::new(),
        ));
        None
    }

    /// 路径里的家目录写成 `~`（Puffin `tilde`）。
    pub(crate) fn tilde(&self, path: &str) -> String {
        match self.home.as_deref().filter(|h| !h.is_empty()) {
            Some(home) if path == home => "~".to_owned(),
            Some(home) => path
                .strip_prefix(home)
                .filter(|rest| rest.starts_with(['/', '\\']))
                .map_or_else(|| path.to_owned(), |rest| format!("~{rest}")),
            None => path.to_owned(),
        }
    }

    /// 草稿变成 `/export <缺省文件名>`：会话第一条输入加本地时间（`now_ms` 与那一刻的 UTC 偏移由 runner 从注入的时钟取，
    /// time-zone.md §3）。在草稿里改名，⏎ 存（Puffin）。
    pub(crate) fn open_export_name(&mut self, now_ms: u64, offset_minutes: i32) {
        let (_, first) = conversation_text(self, now_ms);
        let name = default_name(first.as_deref(), now_ms, offset_minutes);
        self.composer.set_text(&format!("{EXPORT_DRAFT}{name}"));
    }
}

#[cfg(test)]
#[path = "export_tests.rs"]
mod tests;
