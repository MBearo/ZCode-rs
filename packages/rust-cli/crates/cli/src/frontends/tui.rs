//! 交互界面的入口与接线（docs/specs/rust-cli/tui.md §2；cli-args.md §14.3）：组装（与 `-p` 共用 [`crate::assemble`]），
//! 夹在阶段之间的是更新提示与工作区信任（启动前的两屏，[`super::tui_screens`]）；再把启动时捕获的事实整理成 TUI 的参数
//! （[`super::tui_inputs`]）。TUI 不读进程环境，这里只把它需要的终端与语言变量交给它。

use std::sync::Arc;

use rust_cli_base::secret::EnvSnapshot;
use rust_cli_platform::local::Recorder;
use rust_cli_tui::{Options, QuestionTimeout, Services, UiConfig};

use super::tui_inputs::{self, CompactCell, HistoryWriter, Inputs};
use super::tui_screens::Screens;

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

/// TUI 的 `Options`。设置页（tui-kit.md §4.2「设置」）读写 `compact_at`、规则文件、`--add-dir`。
pub(crate) fn options(
    inputs: Inputs<'_>,
    history: Vec<String>,
    recorder: Recorder,
    version: &str,
) -> Options {
    // `/usage` 的卡与详情页（一分钟的缓存，platform.md §11）。
    let plan_usage: Arc<dyn rust_cli_tui::services::PlanUsage> = Arc::new(
        super::tui_usage::UsagePort::new(inputs.account.clone(), inputs.clock.clone()),
    );
    let (compact_at, compact_pinned) = inputs.compact_at;
    Options {
        history,
        recorder: Some(Arc::new(HistoryWriter(recorder))),
        home: Some(inputs.home.to_string_lossy().into_owned()),
        workspace: inputs.workspace,
        selection: inputs.selection,
        mode: inputs.mode,
        started: inputs.started,
        version: version.to_owned(),
        vars: tui_inputs::vars(inputs.env),
        models: inputs.models,
        startup: inputs.startup,
        fork: inputs.fork,
        stop: inputs.stop,
        ui: UiConfig {
            locale: inputs.locale,
            memory_files: tui_inputs::memory_files(inputs.cwd, inputs.home),
            update: inputs.update,
            question_timeout: question_timeout(inputs.question_timeout, inputs.env),
            mcp_machines: inputs.machines.1,
            rule_files: inputs.rule_files,
            add_dirs: inputs.add_dirs,
            shell_name: Some(tui_inputs::shell_name(inputs.env)),
            // 有 SSH，或 Linux 上没有图形界面（`tui_inputs::remote_shell`）。
            remote_shell: tui_inputs::remote_shell(inputs.env),
        },
        services: Services {
            locale_store: Arc::new(tui_inputs::ConfigLocale {
                paths: inputs.paths.clone(),
            }),
            account: Arc::new(super::tui_account::PlatformAccount {
                platform: inputs.account.clone(),
            }),
            // 驾驶舱的笔记、监听与 tmux（tui-cockpit.md §1）。
            cockpit: Some(Arc::new(super::tui_cockpit::LocalCockpit::new(
                &inputs.paths,
                inputs.env,
            ))),
            // `theme.json`（tui-kit.md §2.3、§3.4）。
            stores: tui_inputs::stores(&inputs.paths),
            preferences: Arc::new(tui_inputs::ConfigPreferences {
                paths: inputs.paths,
                platform: inputs.account,
                mcp: Arc::new(inputs.mcp),
            }),
            editor: Arc::new(super::editor::ProcessEditor::from_env(
                inputs.env,
                inputs.temp,
                inputs.ids.clone(),
            )),
            // 读图片直接经 `Clipboard::image`（`clipboard/image.rs`）。
            clipboard: Arc::new(super::clipboard::SystemClipboard {
                wayland: tui_inputs::has(inputs.env, rust_cli_base::names::env::WAYLAND_DISPLAY),
                display: tui_inputs::has(inputs.env, rust_cli_base::names::env::DISPLAY),
                ssh: tui_inputs::has(inputs.env, rust_cli_base::names::env::SSH_CONNECTION),
                tmux: tui_inputs::has(inputs.env, rust_cli_base::names::env::TMUX),
                wsl: cfg!(target_os = "linux")
                    && tui_inputs::has(inputs.env, rust_cli_base::names::env::WSL_DISTRO_NAME),
            }),
            trust: inputs.trust,
            machines: inputs.machines.0,
            compact_at: Some((Arc::new(CompactCell(compact_at)), compact_pinned)),
            plan_usage: Some(plan_usage),
            // `@` 面板的预览：只读本机工作区里的，远端的没有读文件的接缝，回答 `Missing`（`tui_peek.rs`）。
            peek_file: Some(Arc::new(
                |workspace: rust_cli_kernel_api::env::WorkspaceRef, path: String| {
                    Box::pin(super::tui_peek::peek(workspace, path))
                },
            )),
            file_len: Some(Arc::new(|path: String| {
                Box::pin(async move {
                    rust_cli_fs::file_len(std::path::Path::new(&path))
                        .await
                        .ok()
                        .flatten()
                })
            })),
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
    let vars = tui_inputs::vars(env);
    let os = facts.controller.os;
    let locale = opened.config.locale;
    let updates = crate::assemble::setup::updates(
        env,
        &opened.config,
        &opened.paths,
        (&opened.egress, clock),
    )
    .await;
    // 启动前的两屏（tui-kit.md §2.3）。
    let paths_for_screens = opened.paths.clone();
    let screens = Screens {
        vars: &vars,
        locale,
        paths: &paths_for_screens,
    };
    let ask = async |info| screens.ask_update(info).await;
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
        let ask = async |prompt| screens.ask_trust(prompt).await;
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
        started: facts.process.started,
        mode: built.mode,
        models: opened.directory.clone(),
        startup: tui_inputs::startup(&invocation.target, invocation.resume_pick),
        fork: invocation.fork_session,
        locale,
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
        compact_at: (
            built.compact_at.clone(),
            opened.config.compact_pinned.clone(),
        ),
        machines: (
            built
                .machines
                .as_ref()
                .and_then(crate::machines::Machines::service),
            built.mcp_machines.clone(),
        ),
        rule_files: opened.config.rule_files.clone(),
        add_dirs: opened.resolved.add_dirs.clone(),
    };
    let exit = rust_cli_tui::run(kernel, options(inputs, loaded, recorder, crate::VERSION)).await;
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
