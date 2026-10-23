//! L30 面板的界面文字（docs/specs/rust-cli/tui-panels.md）：中文与英文。英文照原文（Host 的照抄）。
use super::i18n::Lang;

fn pick(lang: Lang, en: &'static str, zh: &'static str) -> &'static str {
    match lang {
        Lang::En => en,
        Lang::Zh => zh,
    }
}

/// 子代理列表的第一行（主会话）。
pub fn main_line(lang: Lang) -> &'static str {
    pick(lang, "main", "主会话")
}

/// 用时后面的等待标记。
pub fn waiting(lang: Lang) -> &'static str {
    pick(lang, " · waiting", " · 等待中")
}

/// 描述后面的当前活动（L35，stall-visibility.md §4.3）与它是不是错误（error 色）。`left_ms` 是重试或停顿
/// 还要等多久。
pub fn activity(
    lang: Lang,
    activity: &rust_cli_kernel_api::panels::SubagentActivity,
    left_ms: u64,
) -> (String, bool) {
    use rust_cli_kernel_api::panels::SubagentActivity;
    let left = super::retry_status::countdown(left_ms);
    match activity {
        // 修复线 F26（用户决定）：不显示“思考中”；修复线 F62（用户决定）：不显示工具名（原来是 ` · <工具名>`，Explore
        // 的 Bash 只跑几十毫秒，一闪而过）。agents 也不再产出这两种；kernel-api 的变体只加不改，保留。
        SubagentActivity::Thinking | SubagentActivity::Tool { .. } => (String::new(), false),
        SubagentActivity::Retrying {
            attempt,
            max_retries,
            ..
        } => (
            match lang {
                Lang::En => format!(" · Retrying in {left} · attempt {attempt}/{max_retries}"),
                Lang::Zh => format!(" · {left}后重试 · 第 {attempt}/{max_retries} 次"),
            },
            true,
        ),
        SubagentActivity::Stalled { .. } => (
            pick(lang, " · Waiting for API response", " · 等待 API 响应").to_owned(),
            true,
        ),
    }
}

/// 列表放不下的子代理数（窗口下面）。
pub fn more(lang: Lang, n: usize) -> String {
    match lang {
        Lang::En => format!("  ↓ {n} more"),
        Lang::Zh => format!("  ↓ 还有 {n} 个"),
    }
}

/// 窗口上面还有的子代理数（main 行的行尾，修复线 F30）。
pub fn more_above(lang: Lang, n: usize) -> String {
    match lang {
        Lang::En => format!("↑ {n} more"),
        Lang::Zh => format!("↑ 还有 {n} 个"),
    }
}

/// 有隐藏的已完成后台代理时 footer 的提示（按键提示模板，tui-panels.md §1.4）。
pub fn tasks_hint(lang: Lang) -> &'static str {
    pick(lang, "{/tasks} to see subagents", "{/tasks} 查看子代理")
}

/// 列表获得焦点时 footer 左侧的提示（按键提示模板）；`stop` 为真时选中项能停止。
pub fn agents_hint(lang: Lang, stop: bool) -> &'static str {
    match (lang, stop) {
        (Lang::En, false) => "{↑/↓} to select · {enter} to view · {esc} to close",
        (Lang::En, true) => "{↑/↓} to select · {enter} to view · {x} to stop · {esc} to close",
        (Lang::Zh, false) => "{↑/↓} 选择 · {enter} 查看 · {esc} 关闭",
        (Lang::Zh, true) => "{↑/↓} 选择 · {enter} 查看 · {x} 停止 · {esc} 关闭",
    }
}

/// 查看子代理时输入框的占位文字（Host §8.4）。
/// 查看子代理时输入框的占位文字（tui-panels.md §1.6、D17，修复线 F62）：写 `@<类型>`，并写明只读（查看时不能给
/// 子代理发消息，所以不写 `Message`）。`kind` 已截到 20 列。
pub fn viewing_placeholder(lang: Lang, kind: &str) -> String {
    match lang {
        Lang::En => format!("Viewing @{kind} (read-only) · esc to return"),
        Lang::Zh => format!("正在查看 @{kind}（只读）· esc 返回"),
    }
}

// ← 项目页的文字（F29 起）在 i18n_projects.rs；这里留项目页也用的几项。
pub fn filter_hint(lang: Lang) -> &'static str {
    pick(lang, " {tab}/{⇧tab} filter", " {tab}/{⇧tab} 筛选")
}

pub fn current(lang: Lang) -> &'static str {
    pick(lang, " (current)", "（当前）")
}

pub fn untitled(lang: Lang) -> &'static str {
    pick(lang, "(untitled)", "（无标题）")
}

pub fn no_sessions(lang: Lang) -> &'static str {
    pick(lang, "No sessions", "没有会话")
}

pub fn last_message(lang: Lang) -> &'static str {
    pick(lang, "Last message", "最近一条回复")
}

/// 选中 pill 时 footer 的提示（Host §8.10）。
pub fn pill_hint(lang: Lang) -> &'static str {
    pick(lang, "{Enter} to view tasks", "{enter} 查看任务")
}
