//! kit 画的 transcript 条目里的字（docs/specs/rust-cli/tui-kit.md §4.2「中文」）：剪去的标记、通知、压缩行与 `Stopped.`、
//! 回合失败的块、工作的收拢的句子、欢迎块的更新提示、挑选回退的点时的切线与 Also / Files。英文用 kit 的缺省（与 Puffin 一字不差）；中文是这里的译文。铺法、
//! 墨色与放不下时怎样缩都在 kit，不随语言变。
use rust_cli_tui_kit::app::element::more_lines;
use rust_cli_tui_kit::app::widgets::entries::{ENTRY_WORDS, EntryWords};
use rust_cli_tui_kit::app::widgets::failure_block::{FAILURE_WORDS, FailureWords, Kind, Names};
use rust_cli_tui_kit::app::widgets::gather::{GATHER_WORDS, GatherWords};
use rust_cli_tui_kit::app::widgets::notices::{NOTICE_WORDS, NoticeWords};
use rust_cli_tui_kit::app::widgets::rewind_rows::{REWIND_WORDS, RewindWords};
use rust_cli_tui_kit::app::widgets::welcome;

use super::i18n::Lang;

/// 剪去的标记：`… N more lines`。
pub(crate) fn more(lang: Lang) -> fn(i64) -> String {
    match lang {
        Lang::En => more_lines,
        Lang::Zh => |n| format!("… 还有 {n} 行"),
    }
}

/// 欢迎块的更新提示：`   X is out · `（后面是 `zcode update`）。
pub(crate) fn is_out(lang: Lang) -> fn(&str) -> String {
    match lang {
        Lang::En => welcome::is_out,
        Lang::Zh => |v| format!("   {v} 已发布 · "),
    }
}

pub(crate) fn notices(lang: Lang) -> &'static NoticeWords {
    match lang {
        Lang::En => &NOTICE_WORDS,
        Lang::Zh => &ZH_NOTICES,
    }
}

const ZH_NOTICES: NoticeWords = NoticeWords {
    notice: "提示",
    warning: "警告",
    command: "命令",
    subagent: "子代理",
    peer: "同伴",
    sent: "已发送",
    finished: "完成了",
    says: "说",
    delivered: "已送达",
    held: "等待批准",
    denied: "被拒绝",
    refused: "被拒收",
    expired: "已过期",
    more: |n| format!("… 还有 {n} 行"),
};

pub(crate) fn entries(lang: Lang) -> &'static EntryWords {
    match lang {
        Lang::En => &ENTRY_WORDS,
        Lang::Zh => &ZH_ENTRIES,
    }
}

const ZH_ENTRIES: EntryWords = EntryWords {
    summarising: "正在总结对话",
    nothing: "没有可压缩的",
    cancelled: "已停止",
    failed: "失败",
    stopped: "已停止。",
    thinking: "思考中",
    thought: "思考了",
};

pub(crate) fn failures(lang: Lang) -> &'static FailureWords {
    match lang {
        Lang::En => &FAILURE_WORDS,
        Lang::Zh => &ZH_FAILURES,
    }
}

/// 句子里不知道 provider 时的说法（Puffin `The provider`）与主机的说法（`the provider`）。
pub(crate) fn the_provider(lang: Lang, capital: bool) -> &'static str {
    match (lang, capital) {
        (Lang::En, true) => "The provider",
        (Lang::En, false) => "the provider",
        (Lang::Zh, _) => "服务商",
    }
}

fn zh_sentence(kind: Kind, n: &Names) -> String {
    let (p, m) = (&n.provider, &n.model);
    match kind {
        Kind::Auth if n.signs_in => format!("{p} 没有接受你的登录。"),
        Kind::Auth => format!("{p} 没有接受你的 API key。"),
        Kind::Limited => format!("{p} 正在限制请求。"),
        Kind::Capped => format!("{p} 暂时停止了这个套餐的请求。"),
        Kind::TooLong => format!("对话对 {m} 来说太长了。"),
        Kind::NotOffered => format!("{p} 的这个套餐不提供 {m}。"),
        Kind::Trouble => format!("{p} 那边出了问题。"),
        Kind::Unreachable | Kind::Proxy => format!("连不上 {}。", n.host),
        Kind::Stalled => "回答停下来了。".to_owned(),
        Kind::Hook => "一个 hook 拦下了你的消息。".to_owned(),
        Kind::Other => "模型请求失败了。".to_owned(),
    }
}

const ZH_FAILURES: FailureWords = FailureWords {
    sentence: zh_sentence,
    tries_again: " 再试一次。",
    signs_in_again: " 重新登录。",
    has_the_key: " 里有密钥。",
    can_switch: " 可以换一个模型。",
    can_switch_then: " 可以换一个模型。 ",
    shortens: " 缩短它。",
    shortens_then: " 缩短它；然后 ",
    picks_another: " 换一个模型。",
    behind_proxy: "在代理后面？设置 ",
    start_again: " 后重新启动 ZCode。",
    edits_kept: "到此为止的修改都保留着。",
    tried: |n| format!("试了 {n} 次"),
    fields: ["代码", "状态", "服务商", "模型", "请求", "时间"],
};

pub(crate) fn gather(lang: Lang) -> &'static GatherWords {
    match lang {
        Lang::En => &GATHER_WORDS,
        Lang::Zh => &ZH_GATHER,
    }
}

/// `k` 个 `noun`（kit 的名词：`file`、`time`、`page`、`command`、`subagent`、`tool`、`call`、`step`）。
fn zh_count(k: i64, noun: &str) -> String {
    match noun {
        "file" => format!("{k} 个文件"),
        "time" => format!("{k} 次"),
        "page" => format!("{k} 个网页"),
        "command" => format!("{k} 个命令"),
        "subagent" => format!("{k} 个子代理"),
        "tool" => format!("{k} 个工具"),
        "call" => format!("{k} 个调用"),
        "step" => format!("{k} 步"),
        _ => format!("{k} 个{noun}"),
    }
}

/// `who` 失败了：名字是 `Bash 失败了`，数是 `1 个失败`。
fn zh_failed(who: &str) -> String {
    if who.chars().all(|c| c.is_ascii_digit()) {
        format!("{who} 个失败")
    } else {
        format!("{who} 失败了")
    }
}

static ZH_GATHER: GatherWords = GatherWords {
    count: zh_count,
    thought: "思考了",
    for_time: |x| format!(" {x}"),
    explored: "浏览了",
    read: "读了",
    searched: "搜索了",
    fetched: "抓取了",
    searched_web: "搜索了网页",
    edited: "编辑了",
    ran: "运行了",
    planned: "计划了",
    checked_off: "完成了",
    called: "调用了",
    stopped: "停下了",
    updated: "更新了",
    the_steps: " 步骤",
    and: "、",
    comma: "，",
    tokens: |x| format!("（{x} tokens）"),
    failed: zh_failed,
    times: |name, k| format!("{name} {k} 次"),
    at_once: |k| format!("同时 {k} 个调用"),
    thinking: "思考中",
};

/// 挑选回退的点时的切线与 Also / Files 两行。
pub(crate) fn rewind(lang: Lang) -> &'static RewindWords {
    match lang {
        Lang::En => &REWIND_WORDS,
        Lang::Zh => &ZH_REWIND,
    }
}

fn zh_stay(n: usize, all: bool) -> String {
    let stay = if all && n > 1 {
        format!("全部 {n} 个回合留在后面")
    } else if n == 1 {
        "下面的回合留在后面".to_owned()
    } else {
        format!("下面的 {n} 个回合留在后面")
    };
    format!("{stay}，在 /resume 里")
}

fn zh_changed(n: usize) -> String {
    if n == 1 {
        "下面的回合改了 ".to_owned()
    } else {
        format!("下面的 {n} 个回合改了 ")
    }
}

const ZH_REWIND: RewindWords = RewindWords {
    start: "开头",
    before_last: "你最后一条消息之前",
    before: |n| format!("你第 {n} 条消息之前"),
    back: |w| format!("回到{w}"),
    cant: |w| format!("回不到{w}"),
    unfinished: "上面的回合没有完成",
    unfinished_short: "上面的回合未完成",
    stay: zh_stay,
    kept: "留在 /resume 里",
    also: "另外",
    files: "文件",
    stay_files: "保持现在的样子",
    changed: zh_changed,
    rewound: "从这里回退",
    as_it_was: |t| format!("“{t}”原样留在 /resume 里"),
};
