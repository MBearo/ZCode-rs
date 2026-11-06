//! `/theme`（docs/specs/rust-cli/tui-kit.md §3.4、§4.2「`/theme`」，工作线 TK 的 C6）：Puffin 的主题列表
//! （docs/puffin-tui.md §4.2「主题」，壳的 `shell/theme_list.rs`）——Terminal 与 17 个具名主题，每个一行（名字、两侧），
//! 正在用的后面 fg3 的 `current`；打字过滤（名字或 id，不分大小写）；移到哪个主题整个屏幕当场换成它（预览，不写文件）；
//! ⏎ 选定：写 `theme.json`（与 Puffin 同一个文件、同一格式，经 `Services.stores`），输入框上方一行 `Theme: 名字.`；esc 换回
//! 打开时的主题。主题的 mode 与 ground 照旧（只换主题）。`ui.theme`（auto / dark / light）不再读写（tui-manage.md §2.1 作废）。
//!
//! 墨色：`Ink::chosen` 按探测到的终端颜色与选择重解；排版缓存里的块带着旧颜色，所以预览与
//! 选定都交给 runner 清缓存、重画（`Manage::Repaint`）。
use crossterm::event::KeyEvent;
use rust_cli_tui_kit::theme::scheme::{Scheme, ThemeChoice};
use rust_cli_tui_kit::theme::schemes;

use super::app::{Action, App, Overlay};
use super::i18n::Lang;
use super::ink::Ink;
use super::list::{Choice, Item, ListView, Outcome, Purpose};
use super::manage::Manage;

/// 选定之后输入框上方的一行（Puffin 的 `Theme: 名字.`）。
pub(crate) fn theme_set(lang: Lang, choice: ThemeChoice) -> String {
    let name = schemes::name(choice.theme);
    match lang {
        Lang::En => format!("Theme: {name}."),
        Lang::Zh => format!("主题：{name}。"),
    }
}

pub(crate) fn theme_failed(lang: Lang, reason: &str) -> String {
    match lang {
        Lang::En => format!("Unable to save the theme: {reason}"),
        Lang::Zh => format!("无法保存主题：{reason}"),
    }
}

/// 头一行的说明（Puffin：rich 时 `shows at once · ⏎ keeps it · esc puts back 原来的`，plain 时说这个终端不显示颜色）。
fn note(lang: Lang, rich: bool, was: &str) -> String {
    let name = schemes::name(was);
    match (lang, rich) {
        (Lang::En, true) => format!("shows at once · ⏎ keeps it · esc puts back {name}"),
        (Lang::En, false) => {
            "this terminal shows no colours; it applies in a truecolour one".to_owned()
        }
        (Lang::Zh, true) => format!("移到即显示 · ⏎ 保留 · esc 换回 {name}"),
        (Lang::Zh, false) => "这个终端不显示颜色；在真彩终端里生效".to_owned(),
    }
}

/// 一个主题的两侧（Puffin `Scheme::sides`）；Terminal 跟随终端。
fn sides(lang: Lang, id: &str) -> String {
    match schemes::by_id(id) {
        Some(scheme) => Scheme::sides(scheme),
        None => match lang {
            Lang::En => "follows your terminal".to_owned(),
            Lang::Zh => "跟随终端".to_owned(),
        },
    }
}

/// 主题列表（打开时选中在用的主题）。
pub fn theme_list_view(lang: Lang, ink: &Ink) -> ListView {
    let was = ink.choice.theme;
    let items = schemes::ids()
        .iter()
        .map(|id| Item {
            label: schemes::name(id).to_owned(),
            description: sides(lang, id),
            current: *id == was,
            value: Choice::Theme((*id).to_owned()),
            ..Item::default()
        })
        .collect();
    let title = match lang {
        Lang::En => "Theme",
        Lang::Zh => "主题",
    };
    ListView::new(Purpose::Theme, title.to_owned(), items, String::new())
        .with_subtitle(note(lang, ink.theme.rich, was))
        .filterable()
}

impl App {
    /// `/theme`：打开主题列表。
    pub(super) fn theme_list(&mut self) -> Option<Action> {
        self.show_list(theme_list_view(self.lang, &self.ink));
        None
    }

    /// 主题列表的按键：移动时预览，esc 换回打开时的主题，⏎ 选定（写回交给 runner）。
    pub(super) fn theme_key(&mut self, key: KeyEvent) -> Option<Action> {
        let Some(Overlay::List(list)) = self.overlay.as_mut() else {
            return None;
        };
        match list.on_key(key) {
            Outcome::Pending => self.theme_preview(),
            Outcome::Closed => {
                let was = list
                    .items
                    .iter()
                    .find(|item| item.current)
                    .and_then(|item| theme_id(&item.value));
                self.overlay = None;
                was.and_then(|id| self.preview_theme(&id))
            }
            Outcome::Chosen { choice, .. } => {
                self.overlay = None;
                self.drop_list_draft(&Purpose::Theme);
                self.set_theme(&theme_id(&choice)?)
            }
        }
    }

    /// 选中一项（⏎ 或数字）：先换上（预览已经是它），再交给 runner 写 `theme.json`。
    pub(super) fn set_theme(&mut self, value: &str) -> Option<Action> {
        let theme = schemes::ids().iter().copied().find(|id| *id == value)?;
        let choice = ThemeChoice {
            theme,
            ..self.ink.choice
        };
        self.apply_theme(choice);
        Some(Action::Manage(Manage::SetTheme(choice)))
    }

    /// 主题列表的焦点移过之后（按键或滚轮，同一条路）：换成焦点上的主题；焦点没变、过滤后没有条目或不是主题列表时
    /// 不做事。
    pub(super) fn theme_preview(&mut self) -> Option<Action> {
        let Some(Overlay::List(list)) = &self.overlay else {
            return None;
        };
        if list.purpose != Purpose::Theme {
            return None;
        }
        let id = focused_id(list)?;
        self.preview_theme(&id)
    }

    /// 预览：换成 `id` 的主题（只在这个界面里，不写文件），交给 runner 清缓存重画。
    fn preview_theme(&mut self, id: &str) -> Option<Action> {
        let theme = schemes::ids().iter().copied().find(|known| *known == id)?;
        if theme == self.ink.choice.theme {
            return None;
        }
        self.apply_theme(ThemeChoice {
            theme,
            ..self.ink.choice
        });
        Some(Action::Manage(Manage::Repaint))
    }

    /// 按 `choice` 重解墨色（终端的事实不变）；排版缓存随后由 runner 清掉。
    pub fn apply_theme(&mut self, choice: ThemeChoice) {
        let colors = self.ink.colors();
        self.ink = Ink::chosen(colors.level, colors.foreground, colors.background, choice);
    }
}

/// 列表里焦点条目的主题 id。
fn focused_id(list: &ListView) -> Option<String> {
    list.visible()
        .get(list.selected)
        .and_then(|item| theme_id(&item.value))
}

/// 主题列表条目的值里的主题 id（kit 配色的 id）。
fn theme_id(choice: &Choice) -> Option<String> {
    match choice {
        Choice::Theme(id) => Some(id.clone()),
        _ => None,
    }
}

#[cfg(test)]
#[path = "slash_theme_tests.rs"]
mod tests;
