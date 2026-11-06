//! 启动前的工作区信任页（workspace-trust.md §2.4、§2.5）：整屏只有问题与两个回答。判定与写记录在入口。迁自已删除的 Puffin
//! 界面的 `shell/kept/trust.rs`（它移植自旧 tui 的 ui/trust_view.rs 第 1–177 行的 `Dialog` 与 trust.rs 的 `drive`）。
//!
//! 画法：回答行与门道的回答同一画法（序号、名字，选中的是品牌色），所以也认数字键 `1` / `2`；按键提示是最后一行（键 fg2、
//! 说明 fg3）。连按 ^C / ^D 退出的提示不到期：没有计时器，下一个别的键清掉它。会话里切项目时的信任框是另一份
//! （`ui::trust_ask`）。
use rust_cli_tui_kit::app::cmd::Cmd;
use rust_cli_tui_kit::app::element::{Element, br, stack};
use rust_cli_tui_kit::app::key::Key;
use rust_cli_tui_kit::app::screen::{App, Screen, Viewport};
use rust_cli_tui_kit::app::sub::Sub;
use rust_cli_tui_kit::app::widgets::kept_rows;
use rust_cli_tui_kit::theme::theme::Theme;

use crate::trust::{TrustAnswer, TrustPrompt};

/// 询问的界面状态：焦点（0 是取消项，缺省在取消项）与连按退出的键（`C` 或 `D`）。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct TrustUi {
    pub focus: usize,
    pub armed: Option<char>,
}

/// 一个键（旧 tui 的 `Dialog::key`）：↑ 到取消项、↓ 到信任项，⏎ 选焦点的项，`1` / `2` 直接选；esc 未信任时退出（0），
/// 后备询问时不带授权继续；^C 或 ^D 连按两次退出（1）。给出回答时询问结束。
pub fn trust_key(ui: &mut TrustUi, backstop: bool, key: &Key) -> Option<TrustAnswer> {
    let chosen = |focus: usize| match (focus, backstop) {
        (1, _) => TrustAnswer::Trusted,
        (_, true) => TrustAnswer::WithoutGrants,
        (_, false) => TrustAnswer::Exit(1),
    };
    if let Key::Ctrl(c @ ('C' | 'D')) = key {
        if ui.armed == Some(*c) {
            return Some(TrustAnswer::Exit(1));
        }
        ui.armed = Some(*c);
        return None;
    }
    ui.armed = None;
    match key {
        Key::Up | Key::Ctrl('P') => ui.focus = 0,
        Key::Down | Key::Ctrl('N') => ui.focus = 1,
        Key::Enter => return Some(chosen(ui.focus)),
        Key::Char('1') => return Some(chosen(0)),
        Key::Char('2') => return Some(chosen(1)),
        Key::Esc if backstop => return Some(TrustAnswer::WithoutGrants),
        Key::Esc => return Some(TrustAnswer::Exit(0)),
        _ => {}
    }
    None
}

/// 询问的行（kit 的 `kept_rows::trust_rows`）：标题与路径、安全检查、能做什么、项目授权（有时）、两个回答、按键提示。
pub fn trust_rows(theme: &Theme, prompt: &TrustPrompt, ui: &TrustUi) -> Vec<Element> {
    let t = kept_rows::Trust {
        path: &prompt.path,
        backstop: prompt.backstop,
        grants: prompt.grants.as_ref().map(|g| kept_rows::Grants {
            count: g.count,
            rules: &g.rules,
            sources: &g.sources,
        }),
    };
    kept_rows::trust_rows(theme, &t, ui.focus, ui.armed)
}

/// 启动前的信任页：一个只有这一页的应用。入口用 kit 的运行时跑它，读最终状态的 `answer`；输入结束（运行时退出而没有
/// 回答）当作 `Exit(1)`。
pub struct TrustScreen {
    pub prompt: TrustPrompt,
    pub theme: Theme,
}

/// [`TrustScreen`] 的状态。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct TrustScreenState {
    pub ui: TrustUi,
    pub answer: Option<TrustAnswer>,
}

impl App for TrustScreen {
    type State = TrustScreenState;
    type Msg = Key;

    fn init(&self) -> (TrustScreenState, Cmd<Key>) {
        (TrustScreenState::default(), Cmd::None)
    }

    fn update(&self, key: Key, state: &mut TrustScreenState) -> Cmd<Key> {
        match trust_key(&mut state.ui, self.prompt.backstop, &key) {
            Some(answer) => {
                state.answer = Some(answer);
                Cmd::Quit
            }
            None => Cmd::None,
        }
    }

    fn subscriptions(&self, _state: &TrustScreenState) -> Vec<Sub<Key>> {
        vec![Sub::on_key_press(|k| Some(k.clone()))]
    }

    fn view(&self, state: &TrustScreenState, _viewport: Viewport) -> Screen {
        let mut rows = vec![br()];
        rows.extend(trust_rows(&self.theme, &self.prompt, &state.ui));
        Screen::new(stack(rows))
    }
}
