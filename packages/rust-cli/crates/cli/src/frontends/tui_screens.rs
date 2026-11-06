//! 交互界面启动前的两屏（[`Screens`]，docs/specs/rust-cli/tui-kit.md §2.3）：工作区信任与自动更新的选择。
use std::collections::BTreeMap;

use anyhow::Result;
use futures_util::stream::BoxStream;
use rust_cli_base::TaskScope;
use rust_cli_base::ui_prefs::UiLocale;
use rust_cli_paths::StoragePaths;
use rust_cli_tui::screens::ScreenEntry;
use rust_cli_tui_ports::trust::{TrustAnswer, TrustPrompt};
use rust_cli_tui_ports::update::{UpdateChoice, UpdateInfo};

use crate::frontends::tui_inputs;

/// 启动前的两屏（工作区信任、自动更新的选择）：`rust_cli_tui::screens`（tui-kit.md §2.3），各自进入、离开一次备用屏，
/// 阻塞到回答。主题照 `theme.json`。
pub(crate) struct Screens<'a> {
    /// 终端相关的环境变量（`tui_inputs::vars`）。
    pub vars: &'a BTreeMap<String, String>,
    /// 配置的界面语言（`ui.locale`）：两屏不能进入时的报错照它说（同会话界面）。
    pub locale: Option<UiLocale>,
    pub paths: &'a StoragePaths,
}

impl Screens<'_> {
    /// 更新选择屏（update.md §6）。
    pub(crate) async fn ask_update(&self, info: UpdateInfo) -> Result<UpdateChoice> {
        install_panic_log();
        let scope = TaskScope::root("update-screen", |panic| {
            tracing::error!(task = panic.task, message = %panic.message, "update screen task panicked");
        });
        rust_cli_tui::screens::ask_update(info, self.entry(&scope)).await
    }

    /// 工作区信任页（workspace-trust.md §1）。
    pub(crate) async fn ask_trust(&self, prompt: TrustPrompt) -> Result<TrustAnswer> {
        install_panic_log();
        let scope = TaskScope::root("trust-screen", |panic| {
            tracing::error!(task = panic.task, message = %panic.message, "trust screen task panicked");
        });
        rust_cli_tui::screens::ask_trust(prompt, self.entry(&scope)).await
    }

    /// 两屏要的：终端的环境变量、界面语言、选的主题、自己的任务作用域、resize 信号。
    fn entry(&self, scope: &TaskScope) -> ScreenEntry {
        ScreenEntry {
            vars: self.vars.clone(),
            locale: self.locale,
            stores: tui_inputs::stores(self.paths),
            spawner: scope.spawner(),
            resize: resize_signals(),
        }
    }
}

/// 进两屏之前装 kit 的 panic hook（tui.md §7.9，只装一次）：在原始模式里时 panic 只记日志，不写到屏幕上。
fn install_panic_log() {
    rust_cli_tui_kit::render::panic_log::install(|location, message| {
        let head = rust_cli_base::log_text::head(message);
        tracing::error!(event = "process.panic", location = %location, error = %head, "a thread panicked");
    });
}

/// SIGWINCH（push 式的 resize 由 cli 造）；注册失败时只靠运行时的轮询。
#[cfg(unix)]
fn resize_signals() -> Option<BoxStream<'static, ()>> {
    use futures_util::StreamExt;
    use tokio::signal::unix::{SignalKind, signal};
    let signal = signal(SignalKind::window_change()).ok()?;
    let stream = futures_util::stream::unfold(signal, |mut signal| async move {
        signal.recv().await.map(|()| ((), signal))
    });
    Some(stream.boxed())
}

/// Windows：crossterm 的控制台事件里已经有 resize。
#[cfg(not(unix))]
fn resize_signals() -> Option<BoxStream<'static, ()>> {
    None
}
