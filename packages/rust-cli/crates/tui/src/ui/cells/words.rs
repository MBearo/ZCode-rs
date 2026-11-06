// 复制自 apps/zcode-host/crates/tui/src/ui/cells/words.rs（feat/rs 79b6691c55）。
//! transcript 块里的固定说法（docs/specs/rust-tui.md §8.2）：中文与英文。
use crate::ui::i18n::Lang;

/// `!` 命令收起的输出上面那一行：`… N earlier lines`（Puffin `shell_block::output_rows`）。
pub fn earlier_lines(lang: Lang, count: usize) -> String {
    match lang {
        Lang::En => format!(
            "… {count} earlier line{}",
            if count == 1 { "" } else { "s" }
        ),
        Lang::Zh => format!("… 前面还有 {count} 行"),
    }
}

/// `!` 命令没有输出。
pub fn no_output(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "(No output)",
        Lang::Zh => "（没有输出）",
    }
}

/// 定时任务的触发行（`Running scheduled task (…)` 与 `ZCode resuming /loop wakeup (…)`，
/// tools-more.md §18.3）。`fold` 是（连续次数、开始时间）。
pub fn fire(lang: Lang, at: &str, wakeup: bool, fold: Option<(u32, &str)>) -> String {
    let head = match (lang, wakeup) {
        (Lang::En, false) => format!("Running scheduled task ({at})"),
        (Lang::En, true) => format!("ZCode resuming /loop wakeup ({at})"),
        (Lang::Zh, false) => format!("正在运行定时任务（{at}）"),
        (Lang::Zh, true) => format!("ZCode 恢复 /loop 唤醒（{at}）"),
    };
    let Some((streak, since)) = fold else {
        return head;
    };
    match lang {
        Lang::En => format!(
            "{head} \u{b7} {streak} no-op {} since {since}",
            if streak == 1 { "tick" } else { "ticks" }
        ),
        Lang::Zh => format!("{head} \u{b7} 自 {since} 起连续 {streak} 次无动作"),
    }
}

pub fn full_access(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "Full access",
        Lang::Zh => "完全访问",
    }
}

/// 回合的错误文字（compact.md §2.2）：到了阻塞线的 `Prompt is too long` 换成“上下文满了”的提示；带压缩失败原因的接上
/// “用 /clear 重新开始”。其余原样。
pub fn turn_error(lang: Lang, message: &str) -> String {
    use rust_cli_kernel_api::compaction::PROMPT_TOO_LONG;
    if message == PROMPT_TOO_LONG {
        return match lang {
            Lang::En => "Context limit reached \u{b7} /compact or /clear to continue",
            Lang::Zh => "上下文已满 \u{b7} 用 /compact 或 /clear 继续",
        }
        .to_owned();
    }
    if message.starts_with(&format!("{PROMPT_TOO_LONG} \u{b7} ")) {
        return match lang {
            Lang::En => format!("{message} \u{b7} /clear to start fresh"),
            Lang::Zh => format!("{message} \u{b7} 用 /clear 重新开始"),
        };
    }
    message.to_owned()
}
