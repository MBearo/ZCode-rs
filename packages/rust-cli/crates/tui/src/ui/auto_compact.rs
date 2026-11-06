//! `/autocompact`：看或改对话什么时候压缩（Puffin `shell/auto_compact.rs`，docs/puffin-tui.md §4.2「自动压缩（/autocompact）」；
//! docs/specs/rust-cli/tui.md §6）。设置是 `compact.autoThresholdTokens`，与设置页的「自动压缩」一行同一个值、同一条写入
//! （runner 的 `Manage::AutoCompact`）；会话实际在哪里压缩来自视图的上下文用量。英文照 Puffin，中文是自拟的译文。
use super::app::{Action, App};
use super::i18n::Lang;
use super::manage::Manage;
use super::settings::tokens::exact as t;

/// 去掉设置的词。
const DEFAULT: &str = "default";

/// 没有自动压缩的设置时（测试、没有配置的入口）的原因，同设置页。
pub const UNAVAILABLE: &str = "This isn't available in this version yet";

/// `/autocompact` 的词：`Some(Some(n))` 设成 `n`，`Some(None)` 去掉设置，`None` 不是 token 数。
pub fn parse(words: &str) -> Option<Option<u64>> {
    if words.trim().eq_ignore_ascii_case(DEFAULT) {
        Some(None)
    } else {
        super::settings::tokens::parse(words).map(Some)
    }
}

/// 不是 token 数的词时的 ✗（Puffin `REFUSED`）。
pub fn refused(lang: Lang) -> &'static str {
    match lang {
        Lang::En => {
            "Write a number of tokens, like 400k. /autocompact default goes back to the model's limit."
        }
        Lang::Zh => "写一个 token 数，比如 400k。/autocompact default 回到模型的上限。",
    }
}

/// 会话在哪里压缩：`setting` 是配置，`effective` 是视图知道时会话的门槛。
pub fn shown(lang: Lang, setting: Option<u64>, effective: Option<u64>) -> String {
    match (lang, setting, effective) {
        (Lang::En, None, Some(e)) => format!(
            "This session compacts at {} tokens, the model's limit.",
            t(e)
        ),
        (Lang::Zh, None, Some(e)) => format!("这个会话在 {} tokens 时压缩，是模型的上限。", t(e)),
        (Lang::En, Some(s), Some(e)) if e == s => {
            format!("This session compacts at {} tokens, your setting.", t(e))
        }
        (Lang::Zh, Some(s), Some(e)) if e == s => {
            format!("这个会话在 {} tokens 时压缩，是你的设置。", t(e))
        }
        (Lang::En, Some(s), Some(e)) if e < s => format!(
            "This session compacts at {} tokens, the model's limit (your setting: {}).",
            t(e),
            t(s)
        ),
        (Lang::Zh, Some(s), Some(e)) if e < s => {
            format!(
                "这个会话在 {} tokens 时压缩，是模型的上限（你的设置：{}）。",
                t(e),
                t(s)
            )
        }
        (Lang::En, Some(s), Some(e)) => format!(
            "This session compacts at {} tokens; your setting is {}.",
            t(e),
            t(s)
        ),
        (Lang::Zh, Some(s), Some(e)) => {
            format!("这个会话在 {} tokens 时压缩；你的设置是 {}。", t(e), t(s))
        }
        (Lang::En, Some(s), None) => format!(
            "Sessions compact at {} tokens, or at the model's limit if it is lower.",
            t(s)
        ),
        (Lang::Zh, Some(s), None) => {
            format!("会话在 {} tokens 时压缩；模型的上限更低时按上限。", t(s))
        }
        (Lang::En, None, None) => "Sessions compact at the model's limit.".to_owned(),
        (Lang::Zh, None, None) => "会话在模型的上限压缩。".to_owned(),
    }
}

/// 存下的设置说什么：运行的会话从下一次判定起就按它。
pub fn saved(lang: Lang, at: Option<u64>) -> String {
    match (lang, at) {
        (Lang::En, None) => "Saved. Sessions compact at the model's limit.".to_owned(),
        (Lang::Zh, None) => "已保存。会话在模型的上限压缩。".to_owned(),
        (Lang::En, Some(n)) => format!(
            "Saved. Sessions compact at {} tokens, or at the model's limit if it is lower.",
            t(n)
        ),
        (Lang::Zh, Some(n)) => format!(
            "已保存。会话在 {} tokens 时压缩；模型的上限更低时按上限。",
            t(n)
        ),
    }
}

/// 存下了、但运行的会话不会按它（项目或环境钉住了这个值）。
pub fn unreloaded(lang: Lang, why: &str) -> String {
    match lang {
        Lang::En => format!("Saved, but this session keeps compacting where it did: {why}"),
        Lang::Zh => format!("已保存，但这个会话还在原来的位置压缩：{why}"),
    }
}

pub fn save_failed(lang: Lang, why: &str) -> String {
    match lang {
        Lang::En => format!("Could not save: {why}"),
        Lang::Zh => format!("无法保存：{why}"),
    }
}

pub fn read_failed(lang: Lang, why: &str) -> String {
    match lang {
        Lang::En => format!("Could not read the settings: {why}"),
        Lang::Zh => format!("无法读取设置：{why}"),
    }
}

impl App {
    /// `/autocompact [词]`：没有词时说现在在哪里压缩，否则写设置；不是 token 数的词不写，✗ 说它要什么（Puffin `auto_compact`）。
    pub(super) fn auto_compact_command(&mut self, args: &str) -> Option<Action> {
        if args.trim().is_empty() {
            return Some(Action::Manage(Manage::AutoCompact(None)));
        }
        match parse(args) {
            Some(at) => Some(Action::Manage(Manage::AutoCompact(Some(at)))),
            None => {
                self.error(refused(self.lang));
                None
            }
        }
    }
}

#[cfg(test)]
#[path = "auto_compact_tests.rs"]
mod tests;
