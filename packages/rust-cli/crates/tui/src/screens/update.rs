//! 自动更新的选择屏（update.md §6，工作线 F48）：有新版本时在信任页之前问一次，立即更新、跳过或跳过到下一个版本。要不要
//! 弹、读写缓存、执行安装都在入口。迁自已删除的 Puffin 界面的 `shell/kept/update.rs`（它移植自旧 tui 的
//! ui/update_view.rs 第 1–122 行的 `Screen` 与 update.rs 的 `drive`）。
//!
//! 画法：标题行是品牌色的 `Update available` 与 fg3 的版本，选项是门道的回答行（序号、名字与 fg3 的说明，选中的是品牌色），
//! 按键提示是最后一行。选择屏是一个小的 [`App`]（[`UpdateScreen`]），入口用 kit 的运行时跑它，读最终状态的 `choice`；
//! 运行时退出而没有选择时当作 Skip。
use rust_cli_tui_kit::app::cmd::Cmd;
use rust_cli_tui_kit::app::element::{Element, br, stack};
use rust_cli_tui_kit::app::key::Key;
use rust_cli_tui_kit::app::screen::{App, Screen, Viewport};
use rust_cli_tui_kit::app::sub::Sub;
use rust_cli_tui_kit::app::widgets::kept_rows;
use rust_cli_tui_kit::theme::theme::Theme;

use crate::update::{UpdateChoice, UpdateInfo};

pub const CHOICES: [UpdateChoice; 3] = [
    UpdateChoice::UpdateNow,
    UpdateChoice::Skip,
    UpdateChoice::SkipUntilNext,
];

/// 一个键（旧 tui 的 `Screen::key`）：↑↓（与 `k` / `j`）回绕移动，`1`–`3` 直接选，⏎ 选焦点的项，esc 与 ^C / ^D 是 Skip。
pub fn update_key(focus: &mut usize, key: &Key) -> Option<UpdateChoice> {
    let n = CHOICES.len();
    match key {
        Key::Ctrl('C' | 'D') | Key::Esc => return Some(UpdateChoice::Skip),
        Key::Up | Key::Ctrl('P') | Key::Char('k') => *focus = (*focus + n - 1) % n,
        Key::Down | Key::Ctrl('N') | Key::Char('j') => *focus = (*focus + 1) % n,
        Key::Char('1') => return Some(UpdateChoice::UpdateNow),
        Key::Char('2') => return Some(UpdateChoice::Skip),
        Key::Char('3') => return Some(UpdateChoice::SkipUntilNext),
        Key::Enter => return CHOICES.get(*focus).copied(),
        _ => {}
    }
    None
}

/// 选择屏的行（kit 的 `kept_rows::update_rows`）：标题与版本、更新说明的链接、三个选项、按键提示。
pub fn update_rows(theme: &Theme, info: &UpdateInfo, focus: usize) -> Vec<Element> {
    kept_rows::update_rows(
        theme,
        &info.current,
        &info.latest,
        &info.command,
        &info.notes_url,
        focus,
    )
}

/// 选择屏。
pub struct UpdateScreen {
    pub info: UpdateInfo,
    pub theme: Theme,
}

/// [`UpdateScreen`] 的状态：焦点（开始在 Update now）与选出的项。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct UpdateScreenState {
    pub focus: usize,
    pub choice: Option<UpdateChoice>,
}

impl App for UpdateScreen {
    type State = UpdateScreenState;
    type Msg = Key;

    fn init(&self) -> (UpdateScreenState, Cmd<Key>) {
        (UpdateScreenState::default(), Cmd::None)
    }

    fn update(&self, key: Key, state: &mut UpdateScreenState) -> Cmd<Key> {
        match update_key(&mut state.focus, &key) {
            Some(choice) => {
                state.choice = Some(choice);
                Cmd::Quit
            }
            None => Cmd::None,
        }
    }

    fn subscriptions(&self, _state: &UpdateScreenState) -> Vec<Sub<Key>> {
        vec![Sub::on_key_press(|k| Some(k.clone()))]
    }

    fn view(&self, state: &UpdateScreenState, _viewport: Viewport) -> Screen {
        let mut rows = vec![br()];
        rows.extend(update_rows(&self.theme, &self.info, state.focus));
        Screen::new(stack(rows))
    }
}
