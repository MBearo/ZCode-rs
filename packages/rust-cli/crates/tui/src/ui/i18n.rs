// 改写自 apps/zcode-host/crates/tui/src/ui/i18n.rs（feat/rs 79b6691c55）：删掉 V4 / Host 专用的文字。
//! 界面文字（docs/specs/rust-cli/tui.md §7）：中文与英文。语言按配置的 `ui.locale` 与系统语言（§7.1）。
//! 带参数的文字用函数（`i18n_words.rs`、`i18n_account.rs`），词序随语言变化。
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Zh,
    En,
}

impl Lang {
    /// 按界面语言取一句（架构整理 U11：以前 8 个文件各写一份同样的 `pick`）。
    pub fn pick<'a>(self, en: &'a str, zh: &'a str) -> &'a str {
        match self {
            Self::En => en,
            Self::Zh => zh,
        }
    }

    /// Node 的语言代码（`/locale` 的提示用）。
    pub fn code(self) -> &'static str {
        match self {
            Self::Zh => "zh-CN",
            Self::En => "en-US",
        }
    }
}

/// 界面语言（tui.md §7.1，Node `resolveLocale`）：配置的 `zh-CN` / `en-US` 直接用；`auto`、没有配置或其他值
/// 按系统语言，系统语言也认不出时英文。
pub fn resolve(preference: Option<&str>, vars: &BTreeMap<String, String>) -> Lang {
    match preference {
        Some("zh-CN") => Lang::Zh,
        Some("en-US") => Lang::En,
        _ => detect(vars).unwrap_or(Lang::En),
    }
}

/// 系统语言（Node `detectLocale`）：依次看 `LC_ALL`、`LC_MESSAGES`、`LANG`、`LANGUAGE`（冒号分隔的列表），
/// 第一个认得出的值生效；`C`、`POSIX` 与不支持的语言跳过。
fn detect(vars: &BTreeMap<String, String>) -> Option<Lang> {
    [
        rust_cli_base::names::env::LC_ALL,
        rust_cli_base::names::env::LC_MESSAGES,
        rust_cli_base::names::env::LANG,
        rust_cli_base::names::env::LANGUAGE,
    ]
    .into_iter()
    .filter_map(|key| Some((key, vars.get(key)?.trim())))
    .flat_map(|(key, value)| {
        let candidates: Vec<&str> = if key == rust_cli_base::names::env::LANGUAGE {
            value.split(':').map(str::trim).collect()
        } else {
            vec![value]
        };
        candidates
    })
    .find_map(normalize)
}

/// Node `normalizeLocale`：去掉 `.编码` 与 `@修饰`，`_` 换成 `-`，`en` / `en-*` 是英文，`zh` / `zh-*` 是中文。
fn normalize(value: &str) -> Option<Lang> {
    let tag = value
        .split('.')
        .next()?
        .split('@')
        .next()?
        .replace('_', "-");
    let lower = tag.to_ascii_lowercase();
    if lower == "en" || lower.starts_with("en-") {
        Some(Lang::En)
    } else if lower == "zh" || lower.starts_with("zh-") {
        Some(Lang::Zh)
    } else {
        None
    }
}

macro_rules! strings {
    ($($name:ident: $en:literal, $zh:literal;)*) => {
        /// 一种语言的全部固定文字。
        pub struct Strings { $(pub $name: &'static str,)* }
        pub const EN: Strings = Strings { $($name: $en,)* };
        pub const ZH: Strings = Strings { $($name: $zh,)* };
    };
}

strings! {
    requires_terminal: "TUI requires an interactive terminal.", "TUI 需要交互式终端。";
    // Git Bash 的 mintty（tui-input.md §9.6，工作线 F71）。
    mintty_unsupported: "This terminal (mintty, the Git Bash window) cannot run ZCode: its keys do not reach Windows programs. Use Windows Terminal, or run `winpty zcode.cmd`.", "这个终端（mintty，即 Git Bash 的窗口）不能运行 ZCode：按键到不了 Windows 程序。请用 Windows Terminal，或者运行 `winpty zcode.cmd`。";
    placeholder: "Send a message", "发送一条消息";
    no_matches: "no matches", "没有匹配";
    loading: "loading...", "加载中...";
    select_model: "Select model", "选择模型";
    select_model_subtitle: "Applies to this session and new sessions started here.", "用于本会话与之后在这里开始的新会话。";
    no_models: "No model is configured. Add a provider or set ZCODE_API_KEY.", "没有可用的模型。先添加供应商，或设置 ZCODE_API_KEY。";
    copied: "Copied the last reply", "已复制最后一条回复";
    nothing_to_copy: "Nothing to copy yet", "还没有可复制的回复";
    cleared: "Started a new session.", "已开始新会话。";
}

pub fn strings(lang: Lang) -> &'static Strings {
    match lang {
        Lang::Zh => &ZH,
        Lang::En => &EN,
    }
}

#[path = "i18n_words.rs"]
mod words;

#[path = "i18n_account.rs"]
mod account;

pub use account::*;
pub use words::*;

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn the_setting_wins_and_system_follows_the_locale() {
        let zh_env = vars(&[(rust_cli_base::names::env::LANG, "zh_CN.UTF-8")]);
        assert_eq!(resolve(Some("en-US"), &zh_env), Lang::En);
        assert_eq!(resolve(Some("zh-CN"), &vars(&[])), Lang::Zh);
        assert_eq!(resolve(Some("auto"), &zh_env), Lang::Zh);
        assert_eq!(resolve(Some("system"), &zh_env), Lang::Zh);
        assert_eq!(resolve(None, &vars(&[])), Lang::En);
    }

    /// Node `detectLocale`（tui.md §7.1）：`C` / `POSIX` 与不支持的语言跳过，看下一个变量。
    #[test]
    fn the_system_locale_follows_node_detection() {
        let detect = |pairs: &[(&str, &str)]| resolve(None, &vars(pairs));
        assert_eq!(
            detect(&[
                (rust_cli_base::names::env::LC_ALL, "C"),
                (rust_cli_base::names::env::LANG, "zh_CN")
            ]),
            Lang::Zh
        );
        assert_eq!(
            detect(&[
                (rust_cli_base::names::env::LC_ALL, "POSIX"),
                (rust_cli_base::names::env::LANG, "en_US.UTF-8")
            ]),
            Lang::En
        );
        assert_eq!(
            detect(&[(rust_cli_base::names::env::LC_MESSAGES, "zh_TW.Big5@stroke")]),
            Lang::Zh
        );
        assert_eq!(
            detect(&[
                (rust_cli_base::names::env::LANG, "fr_FR.UTF-8"),
                (rust_cli_base::names::env::LANGUAGE, "fr:zh_CN:en")
            ]),
            Lang::Zh
        );
        assert_eq!(
            detect(&[
                (rust_cli_base::names::env::LANG, "en"),
                (rust_cli_base::names::env::LANGUAGE, "zh")
            ]),
            Lang::En
        );
        assert_eq!(
            detect(&[
                (rust_cli_base::names::env::LANG, "de_DE"),
                (rust_cli_base::names::env::LANGUAGE, " ")
            ]),
            Lang::En
        );
        assert_eq!(Lang::Zh.code(), "zh-CN");
    }

    #[test]
    fn every_permission_mode_has_a_label() {
        use rust_cli_kernel_api::permission::PermissionMode;
        assert_eq!(
            mode_label(Lang::En, PermissionMode::Default),
            "Ask before changes"
        );
        assert_eq!(mode_label(Lang::Zh, PermissionMode::Plan), "计划模式");
        assert_eq!(
            mode_label(Lang::En, PermissionMode::BypassPermissions),
            "Full access"
        );
    }
}
