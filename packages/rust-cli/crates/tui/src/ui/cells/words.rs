// 复制自 apps/zcode-host/crates/tui/src/ui/cells/words.rs（feat/rs 79b6691c55）。
//! transcript 块里的固定说法（docs/specs/rust-tui.md §8.2）：中文与英文。
use crate::ui::i18n::Lang;

fn lines_en(count: usize) -> String {
    if count == 1 {
        "1 line".to_owned()
    } else {
        format!("{count} lines")
    }
}

/// `… +N lines`：只说明还藏了多少行；点击整个块展开（§8.2），不写按键提示。
pub fn more_lines(lang: Lang, count: usize) -> String {
    match lang {
        Lang::En => format!("… +{}", lines_en(count)),
        Lang::Zh => format!("… +{count} 行"),
    }
}

pub fn interrupted(lang: Lang) -> String {
    match lang {
        Lang::En => "Interrupted · What should ZCode do instead?",
        Lang::Zh => "已中断 · 希望 ZCode 改做什么？",
    }
    .to_owned()
}

pub fn waiting(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "Waiting for permission…",
        Lang::Zh => "等待授权…",
    }
}

pub fn running(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "Running…",
        Lang::Zh => "运行中…",
    }
}

pub fn no_output(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "(No output)",
        Lang::Zh => "（没有输出）",
    }
}

/// 细节行里的数字粗体：（前文，数字，后文）。
pub type Counted = (String, String, String);

/// 工作段摘要的一段（§8.2.1）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// 推理用时（已经写成 `6s`、`1m 5s`）。
    Thought(String),
    Search(u64),
    Read(u64),
    List(u64),
    /// （服务，调用次数）。
    Mcp(String, u64),
    Command(u64),
}

/// 工作段摘要各段之间的连接。
pub fn joiner(lang: Lang) -> &'static str {
    match lang {
        Lang::En => ", ",
        Lang::Zh => "，",
    }
}

/// 摘要的一段：（前文，粗体的数，后文）；`active` 时用进行时。英文第一段的首字母由调用方大写。
pub fn segment(lang: Lang, active: bool, part: &Segment) -> Counted {
    let unit = |n: u64, one: &str, many: &str| if n == 1 { one } else { many }.to_owned();
    let pick = |done: &str, doing: &str| if active { doing } else { done }.to_owned();
    match (lang, part) {
        (Lang::En, Segment::Thought(time)) => (
            pick("thought for ", "thinking for "),
            time.clone(),
            String::new(),
        ),
        (Lang::En, Segment::Search(n)) => (
            pick("searched for ", "searching for "),
            n.to_string(),
            unit(*n, " pattern", " patterns"),
        ),
        (Lang::En, Segment::Read(n)) => (
            pick("read ", "reading "),
            n.to_string(),
            unit(*n, " file", " files"),
        ),
        (Lang::En, Segment::List(n)) => (
            pick("listed ", "listing "),
            n.to_string(),
            unit(*n, " directory", " directories"),
        ),
        (Lang::En, Segment::Mcp(server, n)) => {
            let verb = pick("called ", "calling ");
            if *n == 1 {
                (format!("{verb}{server}"), String::new(), String::new())
            } else {
                (format!("{verb}{server} "), n.to_string(), " times".into())
            }
        }
        (Lang::En, Segment::Command(n)) => (
            pick("ran ", "running "),
            n.to_string(),
            unit(*n, " shell command", " shell commands"),
        ),
        (Lang::Zh, Segment::Thought(time)) => {
            (pick("思考了 ", "思考 "), time.clone(), String::new())
        }
        (Lang::Zh, Segment::Search(n)) => {
            (pick("搜索了 ", "搜索 "), n.to_string(), " 个模式".into())
        }
        (Lang::Zh, Segment::Read(n)) => (pick("读取了 ", "读取 "), n.to_string(), " 个文件".into()),
        (Lang::Zh, Segment::List(n)) => (pick("列出了 ", "列出 "), n.to_string(), " 个目录".into()),
        (Lang::Zh, Segment::Mcp(server, n)) => {
            let verb = pick("调用了 ", "调用 ");
            if *n == 1 {
                (format!("{verb}{server}"), String::new(), String::new())
            } else {
                (format!("{verb}{server} "), n.to_string(), " 次".into())
            }
        }
        (Lang::Zh, Segment::Command(n)) => {
            (pick("运行了 ", "运行 "), n.to_string(), " 条命令".into())
        }
    }
}

fn counted(before: &str, count: u64, after: &str) -> Counted {
    (before.to_owned(), count.to_string(), after.to_owned())
}

pub fn read_lines(lang: Lang, count: u64) -> Counted {
    match lang {
        Lang::En => counted("Read ", count, if count == 1 { " line" } else { " lines" }),
        Lang::Zh => counted("读取了 ", count, " 行"),
    }
}

/// `Found N files` / `Found N lines` / `Found N matches` / `Listed N paths`。
pub fn found(lang: Lang, count: u64, unit: &str) -> Counted {
    let one = count == 1;
    match (lang, unit) {
        (Lang::En, "lines") => counted("Found ", count, if one { " line" } else { " lines" }),
        (Lang::En, "matches") => counted("Found ", count, if one { " match" } else { " matches" }),
        (Lang::En, "paths") => counted("Listed ", count, if one { " path" } else { " paths" }),
        (Lang::En, _) => counted("Found ", count, if one { " file" } else { " files" }),
        (Lang::Zh, "lines") => counted("找到 ", count, " 行"),
        (Lang::Zh, "matches") => counted("找到 ", count, " 处匹配"),
        (Lang::Zh, "paths") => counted("列出 ", count, " 个路径"),
        (Lang::Zh, _) => counted("找到 ", count, " 个文件"),
    }
}

/// `Added N lines, removed M lines`；数为 0 的一半不写。返回若干段（前文，数字，后文）。
pub fn changed(lang: Lang, added: u64, removed: u64) -> Vec<Counted> {
    let unit = |n: u64| if n == 1 { " line" } else { " lines" };
    let mut parts = Vec::new();
    match lang {
        Lang::En => {
            if added > 0 {
                parts.push(counted("Added ", added, unit(added)));
            }
            if removed > 0 {
                let lead = if parts.is_empty() {
                    "Removed "
                } else {
                    ", removed "
                };
                parts.push(counted(lead, removed, unit(removed)));
            }
        }
        Lang::Zh => {
            if added > 0 {
                parts.push(counted("新增 ", added, " 行"));
            }
            if removed > 0 {
                let lead = if parts.is_empty() {
                    "删除 "
                } else {
                    "，删除 "
                };
                parts.push(counted(lead, removed, " 行"));
            }
        }
    }
    parts
}

pub fn wrote(lang: Lang, count: u64, path: &str) -> Counted {
    match lang {
        Lang::En => counted(
            "Wrote ",
            count,
            &format!("{} to {path}", if count == 1 { " line" } else { " lines" }),
        ),
        Lang::Zh => counted("写入 ", count, &format!(" 行到 {path}")),
    }
}

pub fn plan(lang: Lang, approved: bool) -> &'static str {
    match (lang, approved) {
        (Lang::En, true) => "User approved ZCode's plan",
        (Lang::En, false) => "User rejected ZCode's plan",
        (Lang::Zh, true) => "已同意 ZCode 的计划",
        (Lang::Zh, false) => "未同意 ZCode 的计划",
    }
}

pub fn compacted(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "Conversation compacted (ctrl+o for history)",
        Lang::Zh => "对话已压缩（ctrl+o 查看历史）",
    }
}

pub fn compacting(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "Compacting conversation…",
        Lang::Zh => "正在压缩对话…",
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
