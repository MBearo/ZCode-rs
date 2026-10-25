//! 入口的组装步骤：平台层、账号命令与扩展（从 main.rs 拆出）。

use super::*;
use rust_cli_kernel_api::clock::Clock as _;
use rust_cli_kernel_api::env_registry::{EnvironmentRegistry, LocalRegistry};
use rust_cli_kernel_api::local_services::LocalServices;
use rust_cli_kernel_api::machine::{Files, MachineEnv};

/// 进程环境（`zcode update` 与启动快照共用，F48）。
#[expect(
    clippy::disallowed_methods,
    reason = "入口 crate 是唯一读取进程环境的位置（spec §4）"
)]
pub(super) fn process_env() -> Vec<(String, String)> {
    std::env::vars_os()
        .map(|(key, value)| {
            let pair = (key.to_string_lossy(), value.to_string_lossy());
            (pair.0.into_owned(), pair.1.into_owned())
        })
        .collect()
}

/// Windows 上 `USERPROFILE` 没有或为空时的 home（Node `os.homedir()` 的系统 API）：`std::env::home_dir()` 在 Rust
/// 1.85 起不看 `HOME`，先 `USERPROFILE`、再 `GetUserProfileDirectoryW`（libuv `uv_os_get_passwd` 用的同一个 API）。
#[cfg(windows)]
#[expect(
    clippy::disallowed_methods,
    reason = "入口 crate 是唯一读取进程环境的位置（spec §4）；Windows 的系统 home 只能经它取"
)]
pub(super) fn windows_profile() -> Option<PathBuf> {
    std::env::home_dir()
}

/// 其他平台不用（paths 自己查系统用户数据库）。
#[cfg(not(windows))]
pub(super) fn windows_profile() -> Option<PathBuf> {
    None
}

/// 存储位置：环境变量给的路径展开 `~`，相对路径相对 cwd（Node `resolveUserPath`）。`storage` 是配置层与
/// 环境变量合并后的 `storage.*`（会话库与存储根，修复线 F2）；读配置之前（运行日志、平台层）用缺省。
pub(super) fn storage_paths(startup: &Startup, storage: &StorageSettings<'_>) -> StoragePaths {
    let resolve = |value: &str| startup.cwd.join(expand_home(value, &startup.home));
    let mut paths = StoragePaths::resolve(storage, &startup.home, &startup.cwd);
    if let Some(base) = &startup.data_base_dir {
        paths = paths.with_data_base_dir(&resolve(base));
    }
    if let Some(file) = &startup.builtin_provider_config {
        paths = paths.with_builtin_provider_config(&resolve(file));
    }
    paths
}

/// 平台层：Provider registry、账号（每请求鉴权、登录）与设置。
pub(super) fn platform(
    startup: &Startup,
    paths: &StoragePaths,
    egress: Arc<Egress>,
    clock: Arc<SystemClock>,
) -> Result<Platform> {
    let secret = CredentialSecret {
        explicit: startup.credential_secret.clone(),
        platform: node_platform().to_owned(),
        // Node `os.homedir()`：与本机存储同一个 home（F57，config.md §2.3）。以前另取一份去掉首尾空白的变量。
        home: startup.home.to_string_lossy().into_owned(),
        username: startup.user_name.clone(),
    };
    let env = |name: &str| non_empty(startup.env.get(name).map(str::to_owned));
    let origins = origins::account_origins(env, VERSION).map_err(anyhow::Error::msg)?;
    let services = AccountServices {
        egress,
        clock,
        origins,
    };
    // `ZCODE_MAX_OUTPUT_TOKENS`：Provider 配置的 `maxOutputTokens.map` 用 min(配置的 max, 它)（cli-args.md §9.1，F42）。
    Ok(Platform::new(paths, &secret, services).with_max_output_tokens(startup.max_output_tokens))
}

/// 网络出口（config.md §2.4，F21）：按环境快照算出口策略；`NODE_EXTRA_CA_CERTS` 用不了时先在 stderr 写警告。
/// 进程里所有出网（模型、WebFetch、MCP HTTP、账号）共用这一个。
pub(super) async fn egress(startup: &Startup) -> Result<Arc<Egress>> {
    let network = network::network(&startup.env, &startup.cwd)
        .await
        .context("configuring network egress")?;
    for warning in &network.warnings {
        eprint_raw(warning);
    }
    let egress = Egress::new(network.policy)
        .await
        .context("configuring network egress")?;
    Ok(Arc::new(egress))
}

/// 自动更新的启动检查（docs/specs/rust-cli/update.md §3，F48）：只在 TUI；其他模式为 None。
pub(super) async fn updates(
    tui: bool,
    startup: &Startup,
    config: &config::ConfigSnapshot,
    paths: &StoragePaths,
    (egress, clock): (&Arc<Egress>, &Arc<SystemClock>),
) -> Option<update::Updates> {
    if !tui {
        return None;
    }
    let inputs = update::Inputs {
        env: &startup.env,
        enabled: config.check_for_update,
        version: VERSION,
        cache: paths.update_cache().as_path().to_path_buf(),
        egress: egress.clone(),
        clock: clock.clone(),
        registry: update::registry(),
    };
    Some(update::Updates::start(inputs).await)
}

/// `zcode login` / `zcode logout`。
pub(super) async fn account(command: login::Command) -> Result<u8> {
    let startup = capture()?;
    let paths = storage_paths(&startup, &StorageSettings::default());
    let egress = egress(&startup).await?;
    let clock = Arc::new(SystemClock::new(startup.zone.clone()));
    let platform = platform(&startup, &paths, egress, clock)?;
    Ok(login::run(command, &platform).await)
}

/// 执行环境的注册表（remote-exec.md §15.8.1）：R0 只有本机——`LocalEnvironment` 实现一台机器的 8 个端口与本机的 Git、
/// 取网页，MCP 传输是同一台机器的 `McpTransport` 端口。R3 加远端机器后，入口与内核读本机文件仍取这里的本机。
pub(super) fn environments(
    env: Arc<LocalEnvironment>,
    mcp: Arc<LocalMcpTransport>,
) -> Arc<dyn EnvironmentRegistry> {
    Arc::new(LocalRegistry::new(
        MachineEnv::new(env.clone(), mcp),
        LocalServices::new(env),
    ))
}

/// `zcode hooks trust …`：工作区文件经本机执行环境读，信任记录经 platform（config.md §4.3）。
pub(super) async fn trust(command: hooks_trust::TrustCommand) -> Result<u8> {
    let startup = capture()?;
    let paths = storage_paths(&startup, &StorageSettings::default());
    let env = LocalEnvironment::new(Some(startup.home.clone()));
    // 记录的时间取自系统时钟（node-store 的 `SystemClock`，F7：入口也不直接读系统时间）。
    let now_ms = SystemClock::new(startup.zone.clone()).now_ms();
    let cwd = startup.cwd.to_string_lossy().into_owned();
    let context = hooks_trust::TrustContext {
        files: &env,
        paths: &paths,
        cwd: &cwd,
        version: VERSION,
        now_ms,
    };
    let output = hooks_trust::run(command, &context).await;
    print_raw(&output.stdout);
    eprint_raw(&output.stderr);
    Ok(output.code)
}

/// skill、MCP 与插件的配置（docs/specs/rust-cli/ext.md §1）：MCP 服务器来自配置文件（`config`）与
/// `--mcp-config`，按 MCP 的机器绑定（旁路文件，remote-exec.md §22.2）设上所在的机器；`--plugin-dir` 相对 cwd，其余
/// 插件候选来自配置快照（ext.md §4.1）。`startup` 是（环境快照、cwd、home）。
pub(super) async fn extensions(
    files: &dyn Files,
    workspace: &WorkspaceRef,
    startup: (&[(String, String)], &std::path::Path, &std::path::Path),
    (flags, allowed): (&mcp::McpFlags, &[String]),
    (plugin_dirs, bindings): (&[String], &rust_cli_paths::StoragePath),
    config: &config::ConfigSnapshot,
) -> Extensions {
    let mut extensions = Extensions::none();
    let (env, cwd, home) = startup;
    // MCP 服务器绑定到哪台机器（R5，remote-exec.md §22.2）：rust-cli 自己的旁路文件，经本机的端口读。
    let path = bindings.as_path().to_string_lossy();
    let bindings = &crate::mcp_machines::McpBindings::load(files, workspace, &path).await;
    let settings = mcp::settings(
        files,
        workspace,
        (flags, allowed),
        (env, bindings),
        config.mcp_servers.clone(),
        config.mcp_enabled,
    )
    .await;
    // 配置里停用的服务器（tui-manage.md §6.1）：MCP 关闭或 `--strict-mcp-config` 时没有；同名的
    // `--mcp-config` 服务器遮住它。
    let mut mcp_disabled: Vec<_> = if config.mcp_enabled && !flags.strict {
        config
            .mcp_disabled
            .iter()
            .filter(|server| !settings.servers.iter().any(|on| on.name == server.name))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    // `/mcp enable` 之后它也连在绑定的机器上（R5，remote-exec.md §22.2）。
    bindings.bind(&mut mcp_disabled);
    extensions.config = ExtConfig {
        mcp_disabled,
        mcp: settings,
        plugin_dirs: plugin_dirs
            .iter()
            .map(|dir| EnvPath::new(cwd.join(expand_home(dir, home)).to_string_lossy()))
            .collect(),
        plugins: config.plugins.clone(),
        workspace: Some(workspace.clone()),
    };
    // stream-json 输入（headless.md §6.1）：Bash 后台命令的缺省期限随之不同。
    extensions.streaming_input = flags.streaming_input;
    extensions
}

/// 要读文件、看目录的参数（cli-args.md §4、§6、§8）。
pub(super) struct FlagInputs<'a> {
    pub(super) system: &'a args::PromptFlags,
    pub(super) append: &'a args::PromptFlags,
    pub(super) add_dirs: &'a [String],
    pub(super) settings: Option<&'a str>,
}

/// 读完的结果：两段 system prompt 的文字、加入的目录、`--settings` 的配置与要写到 stderr 的警告。
pub(super) struct ResolvedFlags {
    pub(super) system_prompt: Option<String>,
    pub(super) append_system_prompt: Option<String>,
    pub(super) add_dirs: Vec<String>,
    pub(super) settings: Option<serde_json::Map<String, serde_json::Value>>,
    pub(super) warnings: Vec<String>,
}

/// 经一个只做文件操作的本机执行环境读文件、看目录（与 `--mcp-config` 同样经执行环境端口）；顺序：
/// system prompt 文件、追加文件、`--add-dir`；`--settings` 更早（启动时预读），出错时同样退出。
pub(super) async fn resolve_flags(
    startup: &Startup,
    workspace: &WorkspaceRef,
    inputs: FlagInputs<'_>,
) -> Result<ResolvedFlags> {
    use rust_cli_kernel_api::env_files::PathStyle;
    let probe = LocalEnvironment::new(Some(startup.home.clone()));
    let cwd = startup.cwd.to_string_lossy();
    let home = startup.home.to_string_lossy();
    let place = flags::Place {
        files: &probe,
        workspace,
        cwd: &cwd,
        home: Some(&home),
        style: if cfg!(windows) {
            PathStyle::Windows
        } else {
            PathStyle::Posix
        },
    };
    let settings = match inputs.settings {
        Some(value) => Some(flags::settings(&place, value).await?),
        None => None,
    };
    let system_prompt =
        flags::prompt_text(&place, inputs.system, flags::PromptKind::System).await?;
    let append_system_prompt =
        flags::prompt_text(&place, inputs.append, flags::PromptKind::Append).await?;
    let added = flags::add_dirs(&place, inputs.add_dirs).await;
    Ok(ResolvedFlags {
        system_prompt,
        append_system_prompt,
        add_dirs: added.dirs,
        settings,
        warnings: added.warnings,
    })
}

/// `--session-id` 的会话 ID（cli-args.md §7）：Node 的写法 `sess_<uuid>`（Node `createSessionId(id)`）；库里已有时按
/// 报 `Session ID <uuid> is already in use.`。
pub(super) async fn fresh_session_id(
    store: &NodeStore,
    uuid: &str,
) -> Result<rust_cli_kernel_api::ids::SessionId> {
    use rust_cli_kernel_api::store::SessionStore as _;
    let id = rust_cli_kernel_api::ids::SessionId::new(format!("sess_{uuid}"));
    if store.load(&id).await.is_ok() {
        return Err(flags::Fatal(format!("Error: Session ID {uuid} is already in use.")).into());
    }
    Ok(id)
}

/// 设备身份的来源（platform.md §10，工作线 F52）：`<dataBaseDir>/.zcode/v2/telemetry-state.json` 的 `deviceMid`，与 App、
/// Node CLI 共用；新值由同一个 `NodeIds` 生成（README §4 的 ID 入口）。第一次请求时才解析，之后进程内同一个值。
pub(super) fn device_id(paths: &StoragePaths, ids: Arc<NodeIds>) -> rust_cli_model::DeviceIdSource {
    use futures_util::FutureExt as _;
    let device = Arc::new(rust_cli_platform::DeviceMid::new(
        paths,
        Arc::new(move || ids.uuid()),
    ));
    Arc::new(move || {
        let device = device.clone();
        async move { device.get().await }.boxed()
    })
}
