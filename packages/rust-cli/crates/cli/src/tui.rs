//! `Mode::Tui` 的接线（docs/specs/rust-cli/tui.md §2）：把启动时捕获的事实整理成 TUI 的参数。TUI 不读
//! 进程环境，这里只把它需要的终端与语言变量交给它。

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use rust_cli_base::names::{DOT_DIR, LOCAL_RULES_FILE, RULES_FILE};
use rust_cli_headless::SessionTarget;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::provider::ModelDirectory;
use rust_cli_node_store::{NodeIds, SystemClock};
use rust_cli_paths::StoragePaths;
use rust_cli_platform::{Recorder, UiLocale, UiTheme};
use rust_cli_tui::ui::afk::QuestionTimeout;
use rust_cli_tui::{LocaleStore, MemoryFile, Options, Preferences, Services, Startup};

/// TUI 用到的环境变量：色深、语言、终端种类（自动复制与键盘增强）。
const VARS: &[&str] = &[
    rust_cli_base::names::env::TERM,
    rust_cli_base::names::env::COLORTERM,
    rust_cli_base::names::env::TERM_PROGRAM,
    rust_cli_base::names::env::TERM_PROGRAM_VERSION,
    rust_cli_base::names::env::KITTY_WINDOW_ID,
    rust_cli_base::names::env::WT_SESSION,
    rust_cli_base::names::env::LANG,
    rust_cli_base::names::env::LC_ALL,
    rust_cli_base::names::env::LC_MESSAGES,
    // 系统语言按 Node `detectLocale` 也看 LANGUAGE（tui.md §7.1）。
    rust_cli_base::names::env::LANGUAGE,
    // 终端通知在 tmux / screen 里要包一层（tui-manage.md §5.2）。
    rust_cli_base::names::env::TMUX,
    rust_cli_base::names::env::STY,
    // 复制的提示与 OSC 52（tui-select.md §3）：SSH、复用器、终端名。
    rust_cli_base::names::env::SSH_CONNECTION,
    rust_cli_base::names::env::ZELLIJ,
    rust_cli_base::names::env::LC_TERMINAL,
    rust_cli_base::names::env::KONSOLE_VERSION,
    rust_cli_base::names::env::GNOME_TERMINAL_SERVICE,
    rust_cli_base::names::env::XTERM_VERSION,
    rust_cli_base::names::env::VTE_VERSION,
    rust_cli_base::names::env::ALACRITTY_LOG,
    // 滚轮每格的行数（tui-select.md §4.2）；速度是 `ZCODE_SCROLL_SPEED`。
    rust_cli_base::names::env::CURSOR_TRACE_ID,
    rust_cli_base::names::env::VSCODE_GIT_ASKPASS_MAIN,
    rust_cli_base::names::env::TERMINAL_EMULATOR,
    rust_cli_base::names::env::ZCODE_SCROLL_SPEED,
];

/// 环境变量有非空的值。
fn has(env: &[(String, String)], name: &str) -> bool {
    rust_cli_base::secret::env_var(env, name).is_some_and(|value| !value.is_empty())
}

pub(crate) fn vars(env: &[(String, String)]) -> BTreeMap<String, String> {
    env.iter()
        .filter(|(key, _)| VARS.contains(&key.as_str()))
        .cloned()
        .collect()
}

/// TUI 的机器服务与 MCP 服务器的机器标记（remote-exec.md §21.2、§22.8）。
pub(crate) type MachineInputs = (
    Option<Arc<dyn rust_cli_tui::machines::Machines>>,
    Vec<(String, String)>,
);

pub(crate) struct Inputs<'a> {
    pub workspace: WorkspaceRef,
    pub cwd: &'a Path,
    pub home: &'a Path,
    pub env: &'a [(String, String)],
    /// 入口的临时目录（外部编辑器的私有目录在它下面，F58）。
    pub temp: &'a Path,
    pub selection: ModelSelection,
    pub mode: PermissionMode,
    pub models: Option<Arc<dyn ModelDirectory>>,
    pub history: Vec<String>,
    pub recorder: Recorder,
    pub startup: Startup,
    pub fork: bool,
    /// 配置文件的 `ui.locale`（config.md §2）与存储位置（写回用）。
    pub locale: Option<UiLocale>,
    /// 配置文件的 `ui.theme`（tui-manage.md §2.1）。
    pub theme: Option<UiTheme>,
    pub paths: StoragePaths,
    /// 平台层：TUI 的登录与退出（tui.md §7.2）。
    pub account: Arc<rust_cli_platform::Platform>,
    /// MCP 服务器的来源文件与写项目级文件的执行环境（tui-manage.md §6.1）。
    pub mcp: crate::tui_mcp::McpConfigFiles,
    /// 退出信号（session-lifecycle.md §3.5）。
    pub stop: tokio_util::sync::CancellationToken,
    /// 与内核同一份的时钟与 ID（README §4，F7）。
    pub clock: Arc<SystemClock>,
    pub ids: Arc<NodeIds>,
    /// 应用内切到别的项目时的信任判定（workspace-trust.md §2.8）。
    pub trust: Arc<dyn rust_cli_tui::services::WorkspaceTrust>,
    /// npm 上有更新的版本时会话里的提示（update.md §7，F48）。
    pub update: Option<rust_cli_tui::update::UpdateInfo>,
    /// 运行的平台（修复线 F65，tui-input.md §9）。
    pub os: rust_cli_tui::Os,
    /// 配置的 `askUserQuestionTimeout`（毫秒，tools-more.md §1.1，工作线 F72）。
    pub question_timeout: Option<u64>,
    /// 远端机器（remote-exec.md §21.2）：`/machines` 与机器状态行；绑定到远端机器的 MCP 服务器（服务器名，机器名；
    /// R5，§22.8）。
    pub machines: MachineInputs,
}

/// 界面语言写回用户级配置文件（tui.md §7.1，platform `write_ui_locale`）。
struct ConfigLocale {
    paths: StoragePaths,
}

impl LocaleStore for ConfigLocale {
    fn save(&self, locale: UiLocale) -> BoxFuture<'static, Result<String, String>> {
        let paths = self.paths.clone();
        async move {
            rust_cli_platform::write_ui_locale(&paths, locale)
                .await
                .map(|file| file.as_path().display().to_string())
                .map_err(|error| error.to_string())
        }
        .boxed()
    }
}

/// 主题与 `/effort` 缺省的写回（tui-manage.md §2）：主题写用户级配置文件的 `ui.theme`，缺省模型写个人
/// Provider 配置（与 Node 同一个文件与字段）；`/mcp enable/disable` 写定义服务器的配置文件（§6.1）。
struct ConfigPreferences {
    paths: StoragePaths,
    platform: Arc<rust_cli_platform::Platform>,
    mcp: Arc<crate::tui_mcp::McpConfigFiles>,
}

impl Preferences for ConfigPreferences {
    fn save_theme(&self, theme: UiTheme) -> BoxFuture<'static, Result<String, String>> {
        let paths = self.paths.clone();
        async move {
            rust_cli_platform::write_ui_theme(&paths, theme)
                .await
                .map(|file| file.as_path().display().to_string())
                .map_err(|error| error.to_string())
        }
        .boxed()
    }

    fn save_default_model(
        &self,
        selection: ModelSelection,
    ) -> BoxFuture<'static, Result<String, String>> {
        let platform = self.platform.clone();
        async move { platform.save_default_model(&selection).await }.boxed()
    }

    fn save_mcp_enabled(
        &self,
        server: String,
        enabled: bool,
    ) -> BoxFuture<'static, Result<Option<String>, String>> {
        let (mcp, paths) = (self.mcp.clone(), self.paths.clone());
        async move { mcp.save(&paths, &server, enabled).await }.boxed()
    }
}

/// `/memory` 的文件（tui-input.md §4.2）：用户级与项目级的规则文件（README §2 的 zcode 体系）。
pub(crate) fn memory_files(cwd: &Path, home: &Path) -> Vec<MemoryFile> {
    let file = |label: &str, display: &str, path: std::path::PathBuf| MemoryFile {
        label: label.to_owned(),
        display: display.to_owned(),
        path: path.display().to_string(),
    };
    vec![
        file(
            "User memory",
            &format!("~/{DOT_DIR}/{RULES_FILE}"),
            rust_cli_paths::user_path(home, "").join(RULES_FILE),
        ),
        file(
            "Project memory",
            &format!("./{RULES_FILE}"),
            cwd.join(RULES_FILE),
        ),
        file(
            "Project memory (local)",
            &format!("./{LOCAL_RULES_FILE}"),
            cwd.join(LOCAL_RULES_FILE),
        ),
    ]
}

/// `-c` / `-r [id]` → TUI 启动时打开的会话。
pub(crate) fn startup(target: &SessionTarget, pick: bool) -> Startup {
    match target {
        _ if pick => Startup::Pick,
        SessionTarget::New => Startup::New,
        SessionTarget::Continue => Startup::Latest,
        SessionTarget::Resume(id) => Startup::Session(id.as_str().to_owned()),
    }
}

/// 提问框的空闲超时（tools-more.md §1.1，工作线 F72）：配置的 `askUserQuestionTimeout` 与两个环境变量
/// `ZCODE_AFK_TIMEOUT_MS` / `ZCODE_AFK_COUNTDOWN_MS`（JS 的整数读法，读不出整数时没设；环境变量压过配置）。
///
/// 负数（工作线 F78，用户决定）：超时是负数时不超时，门槛是负数时不显示倒计时。以前两者都按 0：负的超时
/// 一弹出就到期，与“不超时”的意图相反。
fn question_timeout(setting_ms: Option<u64>, env: &[(String, String)]) -> Option<QuestionTimeout> {
    let timeout = afk_ms(env, rust_cli_base::names::env::ZCODE_AFK_TIMEOUT_MS);
    if timeout.is_some_and(|ms| ms < 0.0) {
        return None;
    }
    // 门槛 0 不显示倒计时：剩下的秒数在到期之前总不小于 1。
    let countdown =
        afk_ms(env, rust_cli_base::names::env::ZCODE_AFK_COUNTDOWN_MS).map(|ms| ms.max(0.0) as u64);
    QuestionTimeout::new(setting_ms, timeout.map(|ms| ms as u64), countdown)
}

/// 一个环境变量的毫秒数（JS 的整数读法，带符号）；没设或读不出整数时 None。
fn afk_ms(env: &[(String, String)], name: &str) -> Option<f64> {
    let (_, value) = env.iter().find(|(key, _)| key == name)?;
    crate::limits::js_int(value)
}

pub(crate) fn options(inputs: Inputs<'_>, version: &str) -> Options {
    Options {
        history: inputs.history,
        recorder: Some(inputs.recorder),
        home: Some(inputs.home.to_string_lossy().into_owned()),
        workspace: inputs.workspace,
        selection: inputs.selection,
        mode: inputs.mode,
        version: version.to_owned(),
        vars: vars(inputs.env),
        models: inputs.models,
        startup: inputs.startup,
        fork: inputs.fork,
        stop: inputs.stop,
        services: Services {
            locale: inputs.locale,
            locale_store: Some(Arc::new(ConfigLocale {
                paths: inputs.paths.clone(),
            })),
            account: Some(Arc::new(crate::tui_account::PlatformAccount {
                platform: inputs.account.clone(),
            })),
            theme: inputs.theme,
            preferences: Some(Arc::new(ConfigPreferences {
                paths: inputs.paths,
                platform: inputs.account,
                mcp: Arc::new(inputs.mcp),
            })),
            editor: Some(Arc::new(crate::editor::ProcessEditor::from_env(
                inputs.env,
                inputs.temp,
                inputs.ids.clone(),
            ))),
            memory_files: memory_files(inputs.cwd, inputs.home),
            clipboard: Some(Arc::new(crate::clipboard::SystemClipboard {
                wayland: has(inputs.env, rust_cli_base::names::env::WAYLAND_DISPLAY),
                display: has(inputs.env, rust_cli_base::names::env::DISPLAY),
                ssh: has(inputs.env, rust_cli_base::names::env::SSH_CONNECTION),
                tmux: has(inputs.env, rust_cli_base::names::env::TMUX),
                wsl: cfg!(target_os = "linux")
                    && has(inputs.env, rust_cli_base::names::env::WSL_DISTRO_NAME),
            })),
            trust: Some(inputs.trust),
            update: inputs.update,
            question_timeout: question_timeout(inputs.question_timeout, inputs.env),
            machines: inputs.machines.0,
            mcp_machines: inputs.machines.1,
        },
        clock: inputs.clock,
        ids: inputs.ids,
        os: inputs.os,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_terminal_and_locale_variables_reach_the_interface() {
        let env = [
            (
                rust_cli_base::names::env::LANG.to_owned(),
                "zh_CN.UTF-8".to_owned(),
            ),
            (
                rust_cli_base::names::env::ZCODE_API_KEY.to_owned(),
                "sk-test".to_owned(),
            ),
            (
                rust_cli_base::names::env::TERM.to_owned(),
                "xterm-256color".to_owned(),
            ),
            (
                rust_cli_base::names::env::TMUX.to_owned(),
                "/tmp/tmux-501/default,1,0".to_owned(),
            ),
            // 滚轮的速度。
            (
                rust_cli_base::names::env::ZCODE_SCROLL_SPEED.to_owned(),
                "5".to_owned(),
            ),
        ];
        let vars = vars(&env);
        assert_eq!(vars.len(), 4);
        assert!(!vars.contains_key(rust_cli_base::names::env::ZCODE_API_KEY));
        assert_eq!(
            vars.get(rust_cli_base::names::env::ZCODE_SCROLL_SPEED)
                .map(String::as_str),
            Some("5")
        );
    }

    /// 提问框空闲超时的环境变量（tools-more.md §1.1，工作线 F72）：JS 的整数读法，读不出整数时没设。
    #[test]
    fn idle_timeout_variables_read_like_integers() {
        let env = |value: &str| {
            vec![(
                rust_cli_base::names::env::ZCODE_AFK_TIMEOUT_MS.to_owned(),
                value.to_owned(),
            )]
        };
        let timeout = |value: &str| question_timeout(None, &env(value)).map(|t| t.timeout_ms);
        assert_eq!(timeout("4000"), Some(4000));
        assert_eq!(timeout(" 1500ms"), Some(1500));
        assert_eq!(timeout("0"), Some(0));
        assert_eq!(timeout("soon"), None);
        assert_eq!(timeout(""), None);
        // 读不出整数时用配置。
        let setting = question_timeout(Some(60_000), &env("soon")).map(|t| t.timeout_ms);
        assert_eq!(setting, Some(60_000));
    }

    /// 负数（工作线 F78，用户决定；以前按 0，一弹出就到期）：超时是负数时不超时，也压过配置；倒计时门槛是负数时
    /// 不显示倒计时（门槛 0），超时照常。
    #[test]
    fn a_negative_idle_timeout_turns_the_timer_off() {
        let env = |name: &str, value: &str| vec![(name.to_owned(), value.to_owned())];
        assert_eq!(
            question_timeout(
                None,
                &env(rust_cli_base::names::env::ZCODE_AFK_TIMEOUT_MS, "-5")
            ),
            None
        );
        assert_eq!(
            question_timeout(
                Some(300_000),
                &env(rust_cli_base::names::env::ZCODE_AFK_TIMEOUT_MS, "-1")
            ),
            None
        );
        let countdown = question_timeout(
            Some(60_000),
            &env(rust_cli_base::names::env::ZCODE_AFK_COUNTDOWN_MS, "-1"),
        )
        .expect("the setting still applies");
        assert_eq!((countdown.timeout_ms, countdown.countdown_ms), (60_000, 0));
    }
}
