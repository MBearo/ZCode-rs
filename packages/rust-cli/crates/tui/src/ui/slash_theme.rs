//! `/theme`（docs/specs/rust-cli/tui-manage.md §2.1）：自动 / 深色 / 浅色，选中后先写回 Node 配置的 `ui.theme`，
//! 成功后按它重建调色板。当前值与探测到的终端背景只在 `App`（界面状态）；持久值只在配置文件。
use super::app::{Action, App};
use super::i18n::Lang;
use super::list::{Item, ListView, Purpose};
use super::manage::Manage;
use super::palette::{BLACK, Palette, Rgb, WHITE};
use crate::services::UiTheme;

/// 主题对应的终端背景：dark 黑底、light 白底，auto 用启动时探测到的背景。
pub fn background(theme: UiTheme, detected: Option<Rgb>) -> Option<Rgb> {
    match theme {
        UiTheme::Dark => Some(BLACK),
        UiTheme::Light => Some(WHITE),
        UiTheme::Auto => detected,
    }
}

pub(crate) fn theme_set(lang: Lang, theme: UiTheme) -> String {
    match lang {
        Lang::En => format!("Theme set to {}", theme.as_str()),
        Lang::Zh => format!("主题已设为 {}", theme.as_str()),
    }
}

pub(crate) fn theme_failed(lang: Lang, reason: &str) -> String {
    match lang {
        Lang::En => format!("Unable to save the theme: {reason}"),
        Lang::Zh => format!("无法保存主题：{reason}"),
    }
}

impl App {
    /// `/theme`：三个选项（Node `ui.theme` 的三个值），当前值打 ✔。
    pub(super) fn theme_list(&mut self) -> Option<Action> {
        let zh = self.lang == Lang::Zh;
        let (title, subtitle) = if zh {
            ("主题", "选择在你的终端里最好看的文字样式")
        } else {
            (
                "Theme",
                "Choose the text style that looks best with your terminal",
            )
        };
        let options = [
            (
                UiTheme::Auto,
                if zh {
                    "自动（跟随终端）"
                } else {
                    "Auto (match terminal)"
                },
            ),
            (UiTheme::Dark, if zh { "深色" } else { "Dark mode" }),
            (UiTheme::Light, if zh { "浅色" } else { "Light mode" }),
        ];
        let items = options
            .into_iter()
            .map(|(theme, label)| Item {
                label: label.into(),
                value: theme.as_str().into(),
                current: theme == self.theme,
                ..Item::default()
            })
            .collect();
        self.overlay = Some(super::app::Overlay::List(
            ListView::new(Purpose::Theme, title.into(), items, String::new())
                .with_subtitle(subtitle.into()),
        ));
        None
    }

    /// 列表里选中一项：交给 runner 写回（写成功后才换调色板）。
    pub(super) fn set_theme(&mut self, value: &str) -> Option<Action> {
        UiTheme::parse(value).map(|theme| Action::Manage(Manage::SetTheme(theme)))
    }

    /// 写回成功（或没有存储）后：按这个主题重建调色板；排版缓存里的块带着旧颜色，runner 随后清掉缓存并重画。
    pub fn apply_theme(&mut self, theme: UiTheme) {
        self.theme = theme;
        self.palette = Palette::new(
            self.palette.level,
            background(theme, self.terminal_background),
        )
        .with_foreground(self.palette.foreground);
    }
}
