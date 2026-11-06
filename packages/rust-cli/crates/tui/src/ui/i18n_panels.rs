//! L30 面板的界面文字（docs/specs/rust-cli/tui-panels.md）：中文与英文。英文照原文（Host 的照抄）。
use super::i18n::Lang;

/// 子代理列表的第一行（主会话）。
pub fn main_line(lang: Lang) -> &'static str {
    lang.pick("main", "主会话")
}

/// 等你回答的子代理的状态（Puffin 名单的 `waiting for you`）。
pub fn waiting(lang: Lang) -> &'static str {
    lang.pick("waiting for you", "等你回答")
}

/// ^T 时会话里没有任务：卡的位置的一行（tui-tasks.md §3.1，同 Puffin `/runs` 没有运行时的那一行）。
pub fn no_tasks(lang: Lang) -> &'static str {
    lang.pick(
        "No tasks in this session. Subagents, background commands and todos show up here.",
        "这个会话里没有任务。子代理、后台命令与待办会出现在这里。",
    )
}

/// 正在看的那一项（Puffin `/resume` 列表的 `current`）。
pub fn viewed(lang: Lang) -> &'static str {
    lang.pick("current", "当前")
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
            lang.pick(" · Waiting for API response", " · 等待 API 响应")
                .to_owned(),
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

/// 查看子代理时输入框的占位文字（Host §8.4）。
/// 查看子代理时输入框的占位文字（tui-panels.md §1.6、D17，修复线 F62）：写 `@<类型>`，并写明只读（查看时不能给
/// 子代理发消息，所以不写 `Message`）。`kind` 已截到 20 列。
pub fn viewing_placeholder(lang: Lang, kind: &str) -> String {
    match lang {
        Lang::En => format!("Viewing @{kind} (read-only) · esc to return"),
        Lang::Zh => format!("正在查看 @{kind}（只读）· esc 返回"),
    }
}
