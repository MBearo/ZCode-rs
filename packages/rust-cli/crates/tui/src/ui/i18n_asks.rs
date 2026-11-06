//! 请求的字（docs/specs/rust-cli/tui-kit.md §4.2「权限询问、提问、计划审批」）：英文与 Puffin 的壳一字不差（kit 的
//! `widgets::asks` 与壳的 `shell/asks.rs`），中文是旧界面的翻译（Puffin 只有英文）。
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_tui_kit::app::widgets::asks::{self, Answer, AnswerKind, basename, data::Ask};
use rust_cli_tui_kit::app::widgets::modes::{self, Mode};
use rust_cli_tui_kit::render::Style;
use rust_cli_tui_kit::theme::theme::Theme;

use super::i18n::Lang;

/// 旧界面的权限模式 → kit 的模式（状态栏与计划的回答用）。
pub fn kit_mode(mode: PermissionMode) -> Mode {
    match mode {
        PermissionMode::Guarded => Mode::Guarded,
        PermissionMode::BypassPermissions => Mode::Yolo,
        PermissionMode::AcceptEdits => Mode::Edit,
        _ => Mode::Build,
    }
}

/// 板头的问题。
pub fn head(lang: Lang, ask: &Ask, path: &str) -> String {
    if lang == Lang::En {
        return asks::head(ask);
    }
    if asks::is_plan(ask) {
        return "按这个计划开始？".to_owned();
    }
    match ask.tool_name.as_str() {
        "Bash" => "运行这条命令？".to_owned(),
        "Edit" => format!("修改 {}？", basename(path)),
        "Write" => format!("写入 {}？", basename(path)),
        "ApplyPatch" => "应用这个补丁？".to_owned(),
        "WebFetch" => "读取这个网页？".to_owned(),
        "WebSearch" => "搜索网页？".to_owned(),
        t => match t.strip_prefix("mcp__").map(|rest| rest.split_once("__")) {
            Some(Some((server, tool))) => format!("使用 {server} · {tool}？"),
            _ => format!("使用 {t}？"),
        },
    }
}

pub fn risky(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "risky",
        Lang::Zh => "有风险",
    }
}

/// 右边的「还有几个在等」。
pub fn more_waiting(lang: Lang, more: usize) -> Option<String> {
    (more > 0).then(|| match lang {
        Lang::En => asks::more_waiting(more),
        Lang::Zh => format!("还有 {more} 个在等"),
    })
}

pub fn recommended(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "recommended",
        Lang::Zh => "推荐",
    }
}

/// 子代理的请求在预览第一行说谁在问；旧会话的询问另写会话（agents.md §4.1.2）。
pub fn who_asks(lang: Lang, who: &str, session: Option<&str>) -> String {
    let who = who.trim();
    let head = match (lang, who) {
        (Lang::En, "") => "A subagent asks".to_owned(),
        (Lang::En, who) => format!("{who} asks"),
        (Lang::Zh, "") => "子代理在问".to_owned(),
        (Lang::Zh, who) => format!("{who} 在问"),
    };
    match (lang, session.map(str::trim)) {
        (_, None) => head,
        (Lang::En, Some("")) => format!("{head} in an earlier session"),
        (Lang::En, Some(title)) => format!("{head} in session \"{title}\""),
        (Lang::Zh, Some("")) => format!("{head}（之前的会话）"),
        (Lang::Zh, Some(title)) => format!("{head}（会话「{title}」）"),
    }
}

/// 回答的字的翻译：kit 的回答（英文）逐段换成中文；英文原样。
pub fn answers(lang: Lang, theme: &Theme, list: Vec<Answer>, mode: PermissionMode) -> Vec<Answer> {
    if lang == Lang::En {
        return list;
    }
    list.into_iter()
        .map(|a| {
            let label = match a.kind {
                AnswerKind::Approve => "批准".to_owned(),
                AnswerKind::Keep => "继续计划".to_owned(),
                AnswerKind::AllowAll => a
                    .label
                    .strip_prefix("Allow all ")
                    .map_or_else(|| a.label.clone(), |n| format!("全部允许 {n} 个")),
                _ => zh(&a.label).unwrap_or(&a.label).to_owned(),
            };
            let desc = if a.kind == AnswerKind::Approve {
                vec![
                    ("运行于 ".to_owned(), theme.fg3),
                    (
                        super::i18n::mode_label(lang, mode).to_owned(),
                        modes::style(theme, kit_mode(mode)),
                    ),
                    ("  ".to_owned(), Style::DEFAULT),
                    ("⇧⇥".to_owned(), theme.fg2),
                    (" 换模式".to_owned(), theme.fg3),
                ]
            } else {
                a.desc
                    .into_iter()
                    .map(|(t, st)| (zh_rule(&t).unwrap_or(t), st))
                    .collect()
            };
            Answer { label, desc, ..a }
        })
        .collect()
}

/// 固定的一句的中文。
fn zh(text: &str) -> Option<&'static str> {
    Some(match text {
        "Allow once" => "允许一次",
        "once each; the amber rows above are the others" => "各一次；上面琥珀色的行是其余几个",
        "Always allow" => "总是允许",
        "in this project" => "在这个项目里",
        "in this session" => "在这个会话里",
        "Full access" => "完全访问",
        "this session stops asking" => "这个会话不再询问",
        "Deny" => "拒绝",
        "or type what to do instead" => "或输入要它改做什么",
        "and send what you typed" => "并发出你输入的话",
        "type what to change" => "输入要改什么",
        "with what you typed" => "带上你输入的话",
        _ => return None,
    })
}

/// 说明里的一段：固定的句子与规则的说法（`edits to f`、`writes to p`）。
fn zh_rule(text: &str) -> Option<String> {
    if let Some(words) = zh(text) {
        return Some(words.to_owned());
    }
    let each: Vec<String> = text
        .split(", ")
        .map(|part| {
            if let Some(f) = part.strip_prefix("edits to ") {
                format!("对 {f} 的修改")
            } else if let Some(p) = part.strip_prefix("writes to ") {
                format!("写入 {p}")
            } else {
                part.to_owned()
            }
        })
        .collect();
    let joined = each.join("、");
    (joined != text).then_some(joined)
}

/// 输入框的灰字。
pub fn placeholder(lang: Lang, ask: &Ask) -> &'static str {
    match (lang, asks::is_question(ask), asks::is_plan(ask)) {
        (Lang::En, true, _) => "Or type your own answer",
        (Lang::En, _, true) => "Tell it what to change",
        (Lang::En, _, _) => "Tell it what to do instead",
        (Lang::Zh, true, _) => "或输入你自己的回答",
        (Lang::Zh, _, true) => "告诉它要改什么",
        (Lang::Zh, _, _) => "告诉它改做什么",
    }
}

/// 槽里的说明（键不翻译）。
pub fn slot(lang: Lang, words: &str) -> &str {
    if lang == Lang::En {
        return words;
    }
    match words {
        "answer" => "回答",
        "stop" => "停止",
        "deny and send" => "拒绝并发出",
        "keep planning" => "继续计划",
        "pick" => "选择",
        "tick" => "勾选",
        "page" => "翻页",
        "read on" => "往下看",
        "always allow" => "总是允许",
        "cancel" => "取消",
        "deny" => "拒绝",
        other => other,
    }
}

/// 提问的空闲倒计时（槽里 `PgDn read on` 的位置，tools-more.md §1.1）。
pub fn countdown(lang: Lang, seconds: u64) -> String {
    match lang {
        Lang::En => asks::countdown(seconds),
        Lang::Zh => format!("{seconds} 秒后自动继续 · 按任意键留下"),
    }
}

/// 在回答的请求没经我们回答就离开（别处答了）。
pub fn answered_elsewhere(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "Answered in another window.",
        Lang::Zh => "已在别的窗口回答。",
    }
}

/// 回答被拒绝。
pub fn could_not_answer(lang: Lang, reason: &str) -> String {
    match lang {
        Lang::En => format!("Could not answer: {reason}"),
        Lang::Zh => format!("没能回答：{reason}"),
    }
}
