//! 界面语言、登录与退出的文字（docs/specs/rust-cli/tui.md §7.1）。英文取 Node 原文，另配中文。
use super::Lang;

const AVAILABLE: &str = "auto, en-US, zh-CN";

/// `/locale`、`/locale status`、`/locale list`（Node `handleLocaleCommand`）。
pub fn locale_status(lang: Lang, current: &str) -> String {
    match lang {
        Lang::En => format!(
            "Current locale: {current}.\nAvailable locales: {AVAILABLE}.\nUse /locale <locale> to switch and persist the UI locale."
        ),
        Lang::Zh => format!(
            "当前语言：{current}。\n可选语言：{AVAILABLE}。\n输入 /locale <语言> 切换并保存界面语言。"
        ),
    }
}

pub fn locale_unsupported(lang: Lang, value: &str) -> String {
    match lang {
        Lang::En => format!("Unsupported locale: {value}. Available locales: {AVAILABLE}."),
        Lang::Zh => format!("不支持的语言：{value}。可选语言：{AVAILABLE}。"),
    }
}

/// 写回成功（Node `formatLocaleSwitchResult`）；用切换后的语言显示。
pub fn locale_switched(lang: Lang, requested: &str, effective: &str, path: &str) -> String {
    match (lang, requested == effective) {
        (Lang::En, true) => format!("Locale switched to {effective}.\nConfig: {path}"),
        (Lang::En, false) => {
            format!("Locale set to {requested}; effective locale is {effective}.\nConfig: {path}")
        }
        (Lang::Zh, true) => format!("界面语言已切换为 {effective}。\n配置文件：{path}"),
        (Lang::Zh, false) => {
            format!("界面语言设为 {requested}，实际生效 {effective}。\n配置文件：{path}")
        }
    }
}

pub fn locale_failed(lang: Lang, reason: &str) -> String {
    match lang {
        Lang::En => format!("Unable to switch locale: {reason}"),
        Lang::Zh => format!("无法切换界面语言：{reason}"),
    }
}

pub fn login_usage(lang: Lang) -> &'static str {
    match lang {
        Lang::En => {
            "Usage: /login [zai-coding-plan|bigmodel-coding-plan|zai-coding-plan-api-key <api-key>|bigmodel-coding-plan-api-key <api-key>]"
        }
        Lang::Zh => {
            "用法：/login [zai-coding-plan|bigmodel-coding-plan|zai-coding-plan-api-key <api-key>|bigmodel-coding-plan-api-key <api-key>]"
        }
    }
}

pub fn logout_usage(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "Usage: /logout",
        Lang::Zh => "用法：/logout",
    }
}
