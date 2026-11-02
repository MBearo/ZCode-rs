//! 用户级配置 `~/.zcode/cli/config.json` 里界面的两个取值（`ui.locale`、`ui.theme`，docs/specs/rust-cli/config.md §1、
//! tui-manage.md §2.1）。平台层读写文件，TUI 按它们选语言与配色；取值只定义在这里（F133，审查 E7：以前在 platform 的
//! `cli_config`，TUI 为它们依赖整个 platform）。

/// 界面语言的配置值（Node `UiLocale`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiLocale {
    Auto,
    EnUs,
    ZhCn,
}

impl UiLocale {
    /// 只认 Node 的三个值（`isUiLocale`）。
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "en-US" => Some(Self::EnUs),
            "zh-CN" => Some(Self::ZhCn),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::EnUs => "en-US",
            Self::ZhCn => "zh-CN",
        }
    }
}

/// 主题的配置值（Node `ui.theme`：`auto` / `dark` / `light`，docs/specs/rust-cli/tui-manage.md §2.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiTheme {
    Auto,
    Dark,
    Light,
}

impl UiTheme {
    /// 只认 Node schema 的三个值。
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "dark" => Some(Self::Dark),
            "light" => Some(Self::Light),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }
}

#[cfg(test)]
#[path = "ui_prefs_tests.rs"]
mod tests;
