// 复制自 apps/zcode-host/crates/tui/src/ui/notice.rs（feat/rs 79b6691c55）。
//! footer 的临时提示与连按两次的按键（docs/specs/rust-tui.md §8.4、§8.6）。提示到时自动消失；连按的
//! 判断只看这条提示：同一种按键在它显示期间（800 ms）再按一次就生效。
use std::time::{Duration, Instant};

use super::app::App;
use super::i18n::Lang;

/// 连按两次的时限。
pub const DOUBLE: Duration = Duration::from_millis(800);
pub const PASTE_FOR: Duration = Duration::from_secs(8);
pub const HINT_FOR: Duration = Duration::from_secs(5);
pub const IMAGE_FOR: Duration = Duration::from_secs(2);

/// 等待第二次按下的按键。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Armed {
    /// ctrl+c：再按一次退出。
    ExitC,
    /// ctrl+d：再按一次退出。
    ExitD,
    /// esc（输入框有内容）：再按一次清空。
    Clear,
    /// esc（输入框为空）：再按一次进入回退浏览。
    Edit,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    pub text: String,
    pub until: Instant,
    pub armed: Option<Armed>,
    /// `text` 是按键提示模板（`{键}` 标出按键，tui.md §7.4）；其余提示里可能有任意文字，按原文显示。
    pub keyed: bool,
}

pub(crate) fn armed_text(lang: Lang, armed: Armed) -> &'static str {
    match (lang, armed) {
        (Lang::En, Armed::ExitC) => "Press {Ctrl-C} again to exit",
        (Lang::En, Armed::ExitD) => "Press {Ctrl-D} again to exit",
        (Lang::En, Armed::Clear) => "{Esc} again to clear",
        (Lang::En, Armed::Edit) => "{Esc} again to edit previous message",
        (Lang::Zh, Armed::ExitC) => "再按一次 {Ctrl-C} 退出",
        (Lang::Zh, Armed::ExitD) => "再按一次 {Ctrl-D} 退出",
        (Lang::Zh, Armed::Clear) => "再按一次 {Esc} 清空",
        (Lang::Zh, Armed::Edit) => "再按一次 {Esc} 编辑上一条输入",
    }
}

pub fn paste_again(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "paste again to expand",
        Lang::Zh => "再粘贴一次展开",
    }
}

pub fn search_history(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "{ctrl+r} to search history",
        Lang::Zh => "{ctrl+r} 搜索历史",
    }
}

pub fn yank_hint(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "{Ctrl+Y} to paste deleted text",
        Lang::Zh => "{Ctrl+Y} 粘回删掉的文字",
    }
}

pub fn no_image(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "No image found in clipboard",
        Lang::Zh => "剪贴板里没有图片",
    }
}

impl App {
    /// 显示一条临时提示，`duration` 后消失。
    pub fn notify(&mut self, text: impl Into<String>, duration: Duration) {
        self.notice = Some(Notice {
            text: text.into(),
            until: Instant::now() + duration,
            armed: None,
            keyed: false,
        });
    }

    /// 显示一条按键提示（模板里的 `{键}` 按按键样式画），`duration` 后消失。
    pub fn notify_keys(&mut self, template: &str, duration: Duration) {
        self.notify(template, duration);
        if let Some(notice) = self.notice.as_mut() {
            notice.keyed = true;
        }
    }

    /// 连按两次的按键：提示期间再按一次返回 true（并清掉提示）；否则显示提示并返回 false。
    pub(super) fn second_press(&mut self, armed: Armed, now: Instant) -> bool {
        if self
            .notice
            .as_ref()
            .is_some_and(|n| n.armed == Some(armed) && now < n.until)
        {
            self.notice = None;
            return true;
        }
        self.notice = Some(Notice {
            text: armed_text(self.lang, armed).to_owned(),
            until: now + DOUBLE,
            armed: Some(armed),
            keyed: true,
        });
        false
    }

    /// 还在显示的提示（按键提示模板去掉标记后的文字）。
    pub fn notice_text(&self) -> Option<String> {
        self.live_notice().map(|n| {
            if n.keyed {
                super::key_hint::plain(&n.text)
            } else {
                n.text.clone()
            }
        })
    }

    /// 还在显示的提示本身。
    pub fn live_notice(&self) -> Option<&Notice> {
        self.notice.as_ref().filter(|n| Instant::now() < n.until)
    }
}
