//! 交互界面的入口与接线（docs/specs/rust-cli/tui.md §2；cli-args.md §14.3）：组装（与 `-p` 共用 [`crate::assemble`]），
//! 夹在阶段之间的是更新提示与工作区信任；再把启动时捕获的事实整理成 TUI 的参数。TUI 不读进程环境，这里只把它需要的
//! 终端与语言变量交给它。

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use rust_cli_base::clock::SystemClock;
use rust_cli_base::names::{DOT_DIR, LOCAL_RULES_FILE, RULES_FILE};
use rust_cli_base::secret::EnvSnapshot;
use rust_cli_base::ui_prefs::{UiLocale, UiTheme};
use rust_cli_headless::SessionTarget;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::provider::ModelDirectory;
use rust_cli_node_store::NodeIds;
use rust_cli_paths::StoragePaths;
use rust_cli_platform::local::Recorder;
use rust_cli_tui::{HistoryRecorder, QuestionTimeout};
use rust_cli_tui::{LocaleStore, MemoryFile, Options, Preferences, Services, Startup, UiConfig};

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
fn has(env: &EnvSnapshot, name: &str) -> bool {
    env.raw(name).is_some()
}

pub(crate) fn vars(env: &EnvSnapshot) -> BTreeMap<String, String> {
    env.vars()
        .iter()
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
    pub env: &'a EnvSnapshot,
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
    pub mcp: crate::frontends::tui_mcp::McpConfigFiles,
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

/// 输入历史的追加（tui.md「输入历史」）：交给 platform `History` 唯一的写者。
struct HistoryWriter(Recorder);

impl HistoryRecorder for HistoryWriter {
    fn record(&self, text: &str, at_ms: u64) {
        self.0.record(text, at_ms);
    }
}

/// 界面语言写回用户级配置文件（tui.md §7.1，platform `write_ui_locale`）。
struct ConfigLocale {
    paths: StoragePaths,
}

impl LocaleStore for ConfigLocale {
    fn save(&self, locale: UiLocale) -> BoxFuture<'static, Result<String, String>> {
        let paths = self.paths.clone();
        async move {
            rust_cli_platform::local::write_ui_locale(&paths, locale)
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
    mcp: Arc<crate::frontends::tui_mcp::McpConfigFiles>,
}

impl Preferences for ConfigPreferences {
    fn save_theme(&self, theme: UiTheme) -> BoxFuture<'static, Result<String, String>> {
        let paths = self.paths.clone();
        async move {
            rust_cli_platform::local::write_ui_theme(&paths, theme)
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
fn question_timeout(setting_ms: Option<u64>, env: &EnvSnapshot) -> Option<QuestionTimeout> {
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
///
/// 修复（F129，审查 E8）：以前在快照里按原样比较名字，绕过了 Windows 上不分大小写的规则（F58）；现在经 `get`。
fn afk_ms(env: &EnvSnapshot, name: &str) -> Option<f64> {
    crate::assemble::limits::js_int(env.get(name)?)
}

pub(crate) fn options(inputs: Inputs<'_>, version: &str) -> Options {
    Options {
        history: inputs.history,
        recorder: Some(Arc::new(HistoryWriter(inputs.recorder))),
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
        ui: UiConfig {
            locale: inputs.locale,
            theme: inputs.theme,
            memory_files: memory_files(inputs.cwd, inputs.home),
            update: inputs.update,
            question_timeout: question_timeout(inputs.question_timeout, inputs.env),
            mcp_machines: inputs.machines.1,
        },
        services: Services {
            locale_store: Arc::new(ConfigLocale {
                paths: inputs.paths.clone(),
            }),
            account: Arc::new(crate::frontends::tui_account::PlatformAccount {
                platform: inputs.account.clone(),
            }),
            preferences: Arc::new(ConfigPreferences {
                paths: inputs.paths,
                platform: inputs.account,
                mcp: Arc::new(inputs.mcp),
            }),
            editor: Arc::new(crate::frontends::editor::ProcessEditor::from_env(
                inputs.env,
                inputs.temp,
                inputs.ids.clone(),
            )),
            clipboard: Arc::new(crate::frontends::clipboard::SystemClipboard {
                wayland: has(inputs.env, rust_cli_base::names::env::WAYLAND_DISPLAY),
                display: has(inputs.env, rust_cli_base::names::env::DISPLAY),
                ssh: has(inputs.env, rust_cli_base::names::env::SSH_CONNECTION),
                tmux: has(inputs.env, rust_cli_base::names::env::TMUX),
                wsl: cfg!(target_os = "linux")
                    && has(inputs.env, rust_cli_base::names::env::WSL_DISTRO_NAME),
            }),
            trust: inputs.trust,
            machines: inputs.machines.0,
        },
        clock: inputs.clock,
        ids: inputs.ids,
        os: inputs.os,
    }
}

/// 交互界面的一次运行。
pub(crate) async fn run(invocation: crate::commands::args::Invocation) -> anyhow::Result<u8> {
    let run = crate::process::ControllerRun::begin(crate::process::Start {
        mode: "tui",
        surface: None,
    })?;
    let result = interactive(invocation, &run).await;
    run.finish(result).await
}

async fn interactive(
    invocation: crate::commands::args::Invocation,
    run: &crate::process::ControllerRun,
) -> anyhow::Result<u8> {
    use crate::assemble::{self, CliInputs, Controller};
    use crate::commands::flags::Fatal;
    use crate::process::output::write_lossy;
    let crate::process::ControllerRun {
        facts,
        paths,
        clock,
        stop,
        model_io,
        ..
    } = run;
    // TUI 不接受 prompt 参数（cli-args.md D8）与 `--session-id`（§7）。
    if invocation.prompt.is_some() {
        return Err(Fatal(
            "Error: a prompt argument needs -p (the interactive interface does not take one)"
                .to_owned(),
        )
        .into());
    }
    if invocation.session_id.is_some() {
        return Err(Fatal(
            "Error: --session-id needs -p (the interactive interface does not take it)".to_owned(),
        )
        .into());
    }
    let spec = Controller::tui(CliInputs::of(&invocation));
    // TUI 的输入历史在 `<dataBaseDir>/.zcode/v2`，不随 `storage.*`。
    let history = rust_cli_platform::local::History::new(paths);
    let opened = assemble::open(&spec, facts, (paths, clock)).await?;
    // 自动更新（docs/specs/rust-cli/update.md §3，F48）：在工作区信任之前；没有启动器标记时什么都不做。`updates`
    // 留到返回：它持有后台刷新的任务作用域。选了 Update now 时不建会话，安装后以它的结果退出。
    let env = &facts.process.env;
    let vars = vars(env);
    let os = facts.controller.os;
    let (locale, theme) = (opened.config.locale, opened.config.theme);
    let updates = crate::assemble::setup::updates(
        env,
        &opened.config,
        &opened.paths,
        (&opened.egress, clock),
    )
    .await;
    let ask = async |info| rust_cli_tui::update::ask(info, &vars, locale, theme, os).await;
    if let Some(code) =
        crate::commands::update::offer(updates.as_ref(), ask, &stop.token, env).await?
    {
        return Ok(code);
    }
    let mut ported = assemble::ports(&spec, facts, opened, (clock, model_io)).await?;
    // 工作区信任（docs/specs/rust-cli/workspace-trust.md §1）：在启动扩展、建会话之前问，拒绝就退出。
    {
        let opened = &ported.opened;
        let inputs = crate::frontends::workspace_trust::Inputs {
            info: opened.primary.info().as_ref(),
            files: opened.primary.files().as_ref(),
            store: opened.store.as_ref(),
            workspace: &opened.workspace,
            grants: opened
                .config
                .project_allow
                .as_ref()
                .and_then(|(source, rules)| {
                    crate::frontends::workspace_trust::grants(source, rules)
                }),
        };
        let ask = async |prompt| rust_cli_tui::trust::ask(prompt, &vars, locale, theme, os).await;
        // 信任对话框期间收到退出信号：放弃对话框（它的终端守卫随之恢复终端），不建会话。
        let outcome = tokio::select! {
            outcome = crate::frontends::workspace_trust::ensure(inputs, ask) => outcome?,
            () = stop.token.cancelled() => return Ok(0),
        };
        match outcome {
            crate::frontends::workspace_trust::Outcome::Continue => {}
            crate::frontends::workspace_trust::Outcome::ContinueWithoutGrants => {
                ported.opened.config.without_project_grants();
            }
            crate::frontends::workspace_trust::Outcome::Exit(code) => return Ok(code),
        }
    }
    let built = assemble::kernel(&spec, facts, ported, clock).await?;
    let opened = &built.opened;
    let workspace = &opened.workspace;
    // 输入历史：启动时读本工作区的，写者任务按发送顺序追加（tui.md「输入历史」）。
    let loaded = history.load(workspace.root.as_str()).await;
    let (recorder, writer) = history.writer(workspace.root.as_str().to_owned());
    let scope = rust_cli_base::TaskScope::root("tui-history", |_| {});
    let writer = scope.spawner().spawn("history-writer", writer);
    let kernel = built.kernel.clone();
    let inputs = Inputs {
        workspace: workspace.clone(),
        cwd: &facts.process.cwd,
        home: &facts.process.home,
        env,
        temp: &facts.process.temp,
        selection: built.selection.clone(),
        mode: built.mode,
        models: opened.directory.clone(),
        history: loaded,
        recorder,
        startup: startup(&invocation.target, invocation.resume_pick),
        fork: invocation.fork_session,
        locale,
        theme,
        paths: opened.paths.clone(),
        account: opened.platform.clone(),
        mcp: crate::frontends::tui_mcp::McpConfigFiles {
            origins: opened.config.mcp_origins.clone(),
            files: opened.primary.files().clone(),
            ids: kernel.ports().ids.clone(),
            workspace: workspace.clone(),
        },
        stop: stop.token.clone(),
        clock: clock.clone(),
        ids: opened.ids.clone(),
        update: updates
            .as_ref()
            .and_then(crate::commands::update::Updates::notice),
        trust: Arc::new(crate::frontends::workspace_trust::InAppTrust {
            info: opened.primary.info().clone(),
            files: opened.primary.files().clone(),
            store: kernel.ports().store.clone(),
            accepted: Arc::default(),
        }),
        os,
        question_timeout: opened.config.question_timeout,
        machines: (
            built
                .machines
                .as_ref()
                .and_then(crate::machines::Machines::service),
            built.mcp_machines.clone(),
        ),
    };
    let exit = rust_cli_tui::run(kernel, options(inputs, crate::VERSION)).await;
    // TUI 退出后入口全部释放：等写者写完剩下的；跨会话的登记与 socket 删掉。
    let _ = writer.await;
    if let Some(peers) = built.peers {
        peers.shutdown().await;
    }
    let exit = exit?;
    // 退出的顺序（session-lifecycle.md §3.5、§6.2）：终端已在 `run` 返回前恢复，先打印恢复提示，再跑 SessionEnd，hook 的
    // 提示写 stderr。缺陷修复（F14）：以前 SessionEnd 在恢复终端之前跑，恢复提示排在 hook 的提示之后。
    // 终端关掉时（SIGHUP）stdout / stderr 已经写不进去：写失败不能 panic，否则 SessionEnd 跑不到（恢复提示与 hook 提示的
    // 写入都吞掉错误）。
    if let Some(session) = &exit.session {
        write_lossy(
            std::io::stdout(),
            &format!("\nResume this session with:\nzcode --resume {session}\n"),
        );
    }
    for notice in exit.ending.run().await {
        write_lossy(std::io::stderr(), &notice);
    }
    Ok(0)
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
        let vars = vars(&EnvSnapshot::from(env.to_vec()));
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
            EnvSnapshot::from(vec![(
                rust_cli_base::names::env::ZCODE_AFK_TIMEOUT_MS.to_owned(),
                value.to_owned(),
            )])
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
        let env =
            |name: &str, value: &str| EnvSnapshot::from(vec![(name.to_owned(), value.to_owned())]);
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
