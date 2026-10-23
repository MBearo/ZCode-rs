//! 界面语言、登录与退出的文字（docs/specs/rust-cli/tui.md §7.1）。英文取 Node 原文，另配中文。
use super::Lang;
use crate::services::{Configured, Family};

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

pub fn locale_unavailable(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "Locale switching is not available in this client.",
        Lang::Zh => "当前客户端不能切换界面语言。",
    }
}

/// 授权地址（Node `emitLoginAuthorizeMessage`）。
pub fn login_open_url(lang: Lang, family: Family, url: &str) -> String {
    let name = family.display_name();
    match lang {
        Lang::En => format!(
            "Open this URL to sign in with {name}:\n\n{url}\n\nAfter authorization, return here and I will finish the login automatically."
        ),
        Lang::Zh => format!(
            "打开这个地址，用 {name} 登录：\n\n{url}\n\n授权完成后回到这里，会自动完成登录。"
        ),
    }
}

/// 登录或手工 key 的结果（Node `formatLoginResult`：Z.AI 浏览器登录；其余 `formatProviderSetupResult`）。
pub fn configured(lang: Lang, result: &Configured) -> String {
    let name = result.family.display_name();
    let model = &result.model;
    let config = &result.config;
    let detailed = result.oauth && result.family == Family::Zai;
    let mut text = match (lang, detailed) {
        (Lang::En, true) => {
            let user = result
                .user
                .as_deref()
                .map(|user| format!(" as {user}"))
                .unwrap_or_default();
            let credentials = result.credentials.as_deref().unwrap_or_default();
            format!(
                "Configured {name} Coding Plan{user}.\nModel: {model}\nCredentials: {credentials}\nModel selection: {config}"
            )
        }
        (Lang::Zh, true) => {
            let user = result
                .user
                .as_deref()
                .map(|user| format!("，账号 {user}"))
                .unwrap_or_default();
            let credentials = result.credentials.as_deref().unwrap_or_default();
            format!(
                "已配置 {name} Coding Plan{user}。\n模型：{model}\n凭据：{credentials}\n模型选择：{config}"
            )
        }
        (Lang::En, false) => {
            format!("Configured {name} Coding Plan.\nModel: {model}\nModel selection: {config}")
        }
        (Lang::Zh, false) => {
            format!("已配置 {name} Coding Plan。\n模型：{model}\n模型选择：{config}")
        }
    };
    if let (true, Some(reason)) = (detailed, &result.browser_failed) {
        match lang {
            Lang::En => text.push_str(&format!("\nBrowser open failed: {reason}")),
            Lang::Zh => text.push_str(&format!("\n打开浏览器失败：{reason}")),
        }
    }
    text
}

pub fn logged_out(lang: Lang, credentials: &str) -> String {
    match lang {
        Lang::En => format!("Logged out from Coding Plan accounts. Credentials: {credentials}"),
        Lang::Zh => format!("已退出 Coding Plan 账号。凭据：{credentials}"),
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

/// 入口没有给账号服务时（Node 的 `… is not available in this client.`）。
pub fn login_unavailable(lang: Lang, family: Family, api_key: bool) -> String {
    let name = family.display_name();
    match (lang, api_key) {
        (Lang::En, false) => format!("{name} Coding Plan login is not available in this client."),
        (Lang::En, true) => "Manual API key setup is not available in this client.".to_owned(),
        (Lang::Zh, false) => format!("当前客户端不能登录 {name} Coding Plan。"),
        (Lang::Zh, true) => "当前客户端不能手动配置 API key。".to_owned(),
    }
}

pub fn logout_unavailable(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "Logout is not available in this client.",
        Lang::Zh => "当前客户端不能退出登录。",
    }
}
