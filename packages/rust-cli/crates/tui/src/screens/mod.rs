//! 启动前的两屏（docs/specs/rust-cli/tui-kit.md §2.3；workspace-trust.md §2.4、§2.5，update.md §6）：工作区信任页与
//! 更新选择屏各自进入、离开一次备用屏，阻塞到用户回答。页面是 [`TrustScreen`] 与 [`UpdateScreen`]，这里用 kit 的运行时
//! 在真实终端上跑它们，读最终状态的回答。迁自已删除的 Puffin 界面（`screens.rs` 与 `shell/kept/{trust,update}.rs`），
//! 画法与行为不变。
//!
//! 主题与 Puffin 会话界面的首帧相同（docs/puffin-tui.md §4.2「主题」）：先读 `theme.json` 的选择，探测有了终端的前景与
//! 背景之后才画；探测落定之前的按键丢掉（[`Probed`] 的 `update`）：页面出现之前敲下的键不算回答（F138-1：以前提前按下的
//! 回车会选中信任页焦点所在的 `No, exit`，进程以退出码 1 退出）。为什么能丢干净：握手进行时用户的字节按到达顺序立即折进
//! 状态；探测的结果经订阅的源送到，排在落定的那一批之后，同一批里输入又先于订阅的投递处理。所以主题到达之前的键都丢掉，
//! 主题到达的那一批同时画出页面，之后的键才算。
//!
//! 进入之前与会话界面同一个判断（[`Console::refusal`]，tui-input.md §9.6，工作线 F71）：不在终端里或在 Git Bash 的
//! mintty 里时报错，不进入。运行时退出而没有回答（^Q、输入结束）时信任页按 `Exit(1)`、更新屏按 `Skip`。
use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::bail;
use futures_util::stream::{self, BoxStream, StreamExt};
use rust_cli_base::Spawner;
use rust_cli_tui_kit::app::cmd::Cmd;
use rust_cli_tui_kit::app::element::stack;
use rust_cli_tui_kit::app::handshake::TerminalProbe;
use rust_cli_tui_kit::app::key::Key;
use rust_cli_tui_kit::app::runtime::{self, RuntimeConfig};
use rust_cli_tui_kit::app::screen::{App, Screen, Viewport};
use rust_cli_tui_kit::app::sub::Sub;
use rust_cli_tui_kit::render::ColorDepth;
use rust_cli_tui_kit::render::real::RealTerminal;
use rust_cli_tui_kit::render::terminal::Terminal;
use rust_cli_tui_kit::theme::scheme::ThemeChoice;
use rust_cli_tui_kit::theme::schemes;
use rust_cli_tui_kit::theme::theme::Theme;
use tokio::sync::watch;

use crate::services::{Stores, UiLocale};
use crate::trust::{TrustAnswer, TrustPrompt};
use crate::ui::console::Console;
use crate::ui::i18n;
use crate::update::{UpdateChoice, UpdateInfo};

mod trust;
mod update;

#[cfg(test)]
mod screens_tests;

pub use trust::{TrustScreen, TrustScreenState, TrustUi, trust_key, trust_rows};
pub use update::{CHOICES, UpdateScreen, UpdateScreenState, update_key, update_rows};

/// 入口给两屏的（TUI 不读环境变量、不读文件）：终端的环境变量（cli 的 `VARS`）、配置的界面语言（报错照它说）、
/// `theme.json` 的存储、运行时的任务作用域，与 OS 的 resize 信号（没有时只靠运行时的轮询）。
pub struct ScreenEntry {
    pub vars: BTreeMap<String, String>,
    pub locale: Option<UiLocale>,
    pub stores: Option<Arc<dyn Stores>>,
    pub spawner: Spawner,
    pub resize: Option<BoxStream<'static, ()>>,
}

/// 工作区信任页（workspace-trust.md §2.4、§2.5）。
pub async fn ask_trust(prompt: TrustPrompt, entry: ScreenEntry) -> anyhow::Result<TrustAnswer> {
    let state = show(
        move |theme| TrustScreen {
            prompt: prompt.clone(),
            theme,
        },
        entry,
    )
    .await?;
    Ok(state.answer.unwrap_or(TrustAnswer::Exit(1)))
}

/// 更新选择屏（update.md §6）。
pub async fn ask_update(info: UpdateInfo, entry: ScreenEntry) -> anyhow::Result<UpdateChoice> {
    let state = show(
        move |theme| UpdateScreen {
            info: info.clone(),
            theme,
        },
        entry,
    )
    .await?;
    Ok(state.choice.unwrap_or(UpdateChoice::Skip))
}

/// 在真实终端上跑一页，返回页面的最终状态。不在终端里（含 mintty）时失败。
async fn show<A, F>(make: F, entry: ScreenEntry) -> anyhow::Result<A::State>
where
    A: App<Msg = Key>,
    A::State: Default,
    F: Fn(Theme) -> A,
{
    let ScreenEntry {
        vars,
        locale,
        stores,
        spawner,
        resize,
    } = entry;
    // 与会话界面同一个判断（runner 的 `run`）：管道、文件与 mintty 都报错，字照界面语言。缺陷修复：迁到 kit 的运行时之后
    // 两屏只看 `RealTerminal::available`（stdin / stdout 是不是终端），mintty 的 pty 算终端，照常进入却读不到按键。
    let lang = i18n::resolve(locale.map(UiLocale::as_str), &vars);
    if let Some(refusal) = Console::current().refusal(i18n::strings(lang)) {
        bail!("{refusal}");
    }
    let choice = crate::runner::theme_choice(stores.as_deref()).await;
    let (probed, themes) = watch::channel(None::<Theme>);
    let probe = {
        let vars = vars.clone();
        TerminalProbe::new(move |report| {
            let colorful = ColorDepth::detect(&vars).colorful();
            let base = Theme::choose(report.fg, report.bg, colorful);
            probed.send_replace(Some(chosen(base, choice)));
        })
    };
    let real = RealTerminal::new(&vars, resize, spawner.clone());
    let config = RuntimeConfig {
        handshake: Some(Box::new(probe)),
        depth: ColorDepth::detect(&vars),
        ..RuntimeConfig::default()
    };
    let app = Probed { make, themes };
    let state = runtime::run(app, Box::new(real) as Box<dyn Terminal>, config, spawner).await?;
    Ok(state.page)
}

/// 颜色为 `base`（终端自己的颜色）的终端为 `choice` 显示的主题（迁自 Puffin 的 `shell/state/terminal.rs` 的 `theme`，
/// 没有外观报告）。
fn chosen(base: Theme, choice: ThemeChoice) -> Theme {
    if !base.rich || choice.theme == schemes::TERMINAL {
        return base;
    }
    Theme::resolve(base.fg, base.bg, true, &choice, None)
}

/// 页面的消息：探测给出的主题，或一个键。
#[derive(Clone, Debug, PartialEq)]
enum Step {
    Probed(Theme),
    Key(Key),
}

/// 页面加上探测：主题到了才画、才收键。
struct Probed<F> {
    make: F,
    themes: watch::Receiver<Option<Theme>>,
}

struct ProbedState<S> {
    theme: Option<Theme>,
    page: S,
}

/// 探测的订阅在页面的订阅里的 key。
const PROBE_KEY: &str = "screen:probe";

impl<A, F> App for Probed<F>
where
    A: App<Msg = Key>,
    A::State: Default,
    F: Fn(Theme) -> A,
{
    type State = ProbedState<A::State>;
    type Msg = Step;

    fn init(&self) -> (Self::State, Cmd<Step>) {
        let state = ProbedState {
            theme: None,
            page: A::State::default(),
        };
        (state, Cmd::None)
    }

    fn update(&self, msg: Step, state: &mut Self::State) -> Cmd<Step> {
        match (msg, state.theme) {
            (Step::Probed(theme), _) => {
                state.theme = Some(theme);
                Cmd::None
            }
            // 探测之前的键不算回答（见模块文档）。
            (Step::Key(_), None) => Cmd::None,
            (Step::Key(key), Some(theme)) => (self.make)(theme)
                .update(key, &mut state.page)
                .map(Step::Key),
        }
    }

    fn subscriptions(&self, state: &Self::State) -> Vec<Sub<Step>> {
        let mut subs = vec![Sub::on_key_press(|key| Some(Step::Key(key.clone())))];
        if state.theme.is_none() {
            let themes = self.themes.clone();
            subs.push(Sub::source(PROBE_KEY, move || probed(themes.clone())));
        }
        subs
    }

    fn view(&self, state: &Self::State, viewport: Viewport) -> Screen {
        match state.theme {
            Some(theme) => (self.make)(theme).view(&state.page, viewport),
            None => Screen::new(stack(Vec::new())),
        }
    }
}

/// 探测的主题：落定之后给一次；运行时先退出时什么也不给。
fn probed(mut themes: watch::Receiver<Option<Theme>>) -> BoxStream<'static, Step> {
    stream::once(async move { themes.wait_for(Option::is_some).await.ok().and_then(|t| *t) })
        .filter_map(futures_util::future::ready)
        .map(Step::Probed)
        .boxed()
}
