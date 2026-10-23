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
    ["LC_ALL", "LC_MESSAGES", "LANG", "LANGUAGE"]
        .into_iter()
        .filter_map(|key| Some((key, vars.get(key)?.trim())))
        .flat_map(|(key, value)| {
            let candidates: Vec<&str> = if key == "LANGUAGE" {
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
    placeholder: "Ask ZCode to do anything", "让 ZCode 做点什么";
    placeholder_queued: "Queued messages will be sent automatically", "排队的消息会自动发出";
    interrupt_hint: "{esc} to interrupt", "{esc} 中断";
    for_shortcuts: "{?} for shortcuts", "{?} 查看快捷键";
    composer_selection_hint: "{ctrl+c} copy · {esc} clear", "{ctrl+c} 复制 · {esc} 取消选择";
    esc_latest: "{esc} latest", "{esc} 回到最新";
    no_matches: "no matches", "没有匹配";
    loading: "loading...", "加载中...";
    running: "Running", "运行中";
    failed: "Failed", "失败";
    page_hint: "{↑/↓} scroll · {esc} close", "{↑/↓} 滚动 · {esc} 关闭";
    status_title: "Status", "状态";
    select_model: "Select model", "选择模型";
    select_model_subtitle: "Applies to this session and new sessions started here.", "用于本会话与之后在这里开始的新会话。";
    help_title: "Slash commands", "斜杠命令";
    help_not_implemented: "(not implemented)", "（未实现）";
    current: "current", "当前";
    no_models: "No model is configured. Add a provider or set ZCODE_API_KEY.", "没有可用的模型。先添加供应商，或设置 ZCODE_API_KEY。";
    copied: "Copied the last reply", "已复制最后一条回复";
    nothing_to_copy: "Nothing to copy yet", "还没有可复制的回复";
    image: "Image", "图片";
    error: "Error", "错误";
    status_version: "Version", "版本";
    status_model: "Model", "模型";
    status_directory: "Directory", "目录";
    status_permissions: "Permissions", "权限";
    status_session: "Session", "会话";
    status_mcp: "MCP servers", "MCP 服务器";
    status_no_session: "not started", "还没有开始";
    cleared: "Started a new session.", "已开始新会话。";
    starting_session: "Starting the session…", "正在开始会话…";
    plan_on: "Plan mode on", "已打开计划模式";
    plan_off: "Plan mode off", "已关闭计划模式";
    interrupted: "Interrupted · What should ZCode do instead?", "已中断 · 要 ZCode 改做什么？";
    no_changes: "No changes", "没有改动";
    // 登录视图（tui.md §7.2，Node `tui.loginSetup` 的中英文）。
    login_title: "Set Up Coding Plan", "配置 Coding Plan";
    login_prompt: "Choose a login or API key setup method.", "选择登录或 API key 配置方式。";
    login_help: "Use {Up/Down} to choose, {Enter} to select.", "使用 {Up/Down} 选择，{Enter} 确认。";
    login_zai_detail: "Open browser login and create a Coding Plan API key.", "打开浏览器登录，并创建 Coding Plan API key。";
    login_bigmodel_detail: "Open browser login; authorization is detected automatically.", "打开浏览器登录，CLI 会自动查询授权结果。";
    login_key_detail: "Paste a Coding Plan API key manually.", "手动粘贴 Coding Plan API key。";
    login_wait_zai: "Waiting for Z.AI authorization", "等待 Z.AI 授权";
    login_wait_zai_detail: "Complete sign-in in your browser. I will continue when authorization finishes.", "请在浏览器里完成登录。授权完成后会继续配置。";
    login_wait_bigmodel: "Waiting for BigModel authorization", "等待 BigModel 授权";
    login_wait_bigmodel_detail: "Complete sign-in in your browser. Authorization is detected automatically.", "请在浏览器里完成登录，授权成功后会自动继续配置。";
    login_wait_status: "Waiting for browser authorization...", "正在等待浏览器授权...";
    login_wait_help: "{Esc} cancels and returns to setup choices.", "按 {Esc} 取消，并返回配置方式选择。";
    login_cancelling: "Cancelling...", "正在取消...";
    login_cancelled: "Login cancelled. Choose a setup method.", "已取消登录。请选择配置方式。";
    login_key_zai: "Enter Z.AI Coding Plan API Key", "输入 Z.AI Coding Plan API Key";
    login_key_bigmodel: "Enter BigModel Coding Plan API Key", "输入 BigModel Coding Plan API Key";
    login_key_hidden: "Paste the key here. It is hidden while typing.", "在这里粘贴 key，输入时会隐藏显示。";
    login_key_placeholder: "Paste API key", "粘贴 API key";
    login_key_status: "Enter the API key, then press Enter.", "输入 API key 后按 Enter。";
    login_key_help: "{Enter} saves the key. {Esc} returns to setup choices.", "按 {Enter} 保存 key，按 {Esc} 返回配置方式选择。";
    login_key_empty: "API key is required.", "API key 不能为空。";
    login_key_cleared: "API key input cleared.", "已清空 API key 输入。";
    login_key_cancelled: "API key entry cancelled. Choose a setup method.", "已取消 API key 输入。请选择配置方式。";
    login_key_saving: "Saving API key...", "正在保存 API key...";
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
        let zh_env = vars(&[("LANG", "zh_CN.UTF-8")]);
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
        assert_eq!(detect(&[("LC_ALL", "C"), ("LANG", "zh_CN")]), Lang::Zh);
        assert_eq!(
            detect(&[("LC_ALL", "POSIX"), ("LANG", "en_US.UTF-8")]),
            Lang::En
        );
        assert_eq!(detect(&[("LC_MESSAGES", "zh_TW.Big5@stroke")]), Lang::Zh);
        assert_eq!(
            detect(&[("LANG", "fr_FR.UTF-8"), ("LANGUAGE", "fr:zh_CN:en")]),
            Lang::Zh
        );
        assert_eq!(detect(&[("LANG", "en"), ("LANGUAGE", "zh")]), Lang::En);
        assert_eq!(detect(&[("LANG", "de_DE"), ("LANGUAGE", " ")]), Lang::En);
        assert_eq!(Lang::Zh.code(), "zh-CN");
    }

    #[test]
    fn durations_use_the_compact_form() {
        assert_eq!(elapsed(0), "0s");
        assert_eq!(elapsed(65), "1m 05s");
        assert_eq!(elapsed(3723), "1h 02m 03s");
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
