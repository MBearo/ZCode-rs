//! Composition shared by `app-server` and `-p`: the workspace context read once
//! at startup and the engine wired to the adapters.
use anyhow::{Context as _, Result};
use std::path::PathBuf;
use std::sync::Arc;
use zcode_cli_core::Engine;
use zcode_cli_core_api::{ModelIdentity, ModelPort, ModelRegistry, RuntimePorts, SessionStore};
use zcode_cli_domain::config::ConfigSnapshot;
use zcode_cli_host::{SystemClock, WorkspaceConfig, WorkspaceContext};
use zcode_cli_model::{config::ModelConfig, provider::HttpModel, registry::Registry};
use zcode_cli_net::{Egress, NetworkPolicy, RuntimeEnv};
use zcode_cli_state::NodeStore;
use zcode_cli_tools::WorkspaceTools;

/// M11 transition: the Node session database is the store when selected
/// (`--node-storage` or `ZCODE_CLI_RUST_NODE_STORAGE=1`).
pub fn node_storage(flag: bool) -> bool {
    flag || std::env::var_os("ZCODE_CLI_RUST_NODE_STORAGE").is_some_and(|v| v == "1")
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// What every entry resolves before touching storage.
pub struct Context {
    pub home: PathBuf,
    /// The path as given (workspace identity and TS import use it unresolved).
    pub requested_cwd: PathBuf,
    pub cwd: PathBuf,
    pub data_dir: PathBuf,
    pub workspace: String,
    pub runtime_env: Arc<RuntimeEnv>,
    pub workspace_config: Arc<WorkspaceConfig>,
    pub startup_config: ConfigSnapshot,
    pub egress: Arc<Egress>,
}

impl Context {
    pub async fn prepare(cwd: Option<PathBuf>, data_dir: Option<PathBuf>) -> Result<Self> {
        let home = home();
        let requested_cwd = match cwd {
            Some(cwd) => cwd,
            None => std::env::current_dir()?,
        };
        let cwd = tokio::fs::canonicalize(&requested_cwd)
            .await
            .context("Workspace unavailable")?;
        let requested_data = data_dir.unwrap_or_else(|| {
            std::env::var_os("ZCODE_CLI_RUST_DATA_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".zcode/rust"))
        });
        let data_dir = if requested_data.is_absolute() {
            requested_data
        } else {
            std::env::current_dir()?.join(requested_data)
        };
        tokio::fs::create_dir_all(&data_dir).await?;
        let data_dir = tokio::fs::canonicalize(data_dir).await?;
        // 身份使用 Host 提交的路径，不把 macOS /var -> /private/var 的 realpath 改写成新工作区。
        let workspace = zcode_cli_host::workspace_identity(
            std::env::var("ZCODE_WORKSPACE_IDENTITY").ok().as_deref(),
            &requested_cwd,
        );
        // 与 Node 启动时净化 process.env 等价：只捕获一次，所有读取方共用这份视图，不改真实进程环境。
        let runtime_env = Arc::new(zcode_cli_rust::runtime_env(&home));
        // 配置按 Node 分层规则从 cwd 解析；各入口每次重新加载，与 Node 一致没有文件监听。
        let workspace_config = Arc::new(WorkspaceConfig::new(
            std::path::absolute(&requested_cwd)?,
            home.clone(),
            runtime_env.vars().to_vec(),
        ));
        // 网络出口与权限配置与 Node 一样取启动时的配置快照，运行中修改不影响已建立的出口。
        let startup_config = workspace_config.snapshot().await;
        let egress = Arc::new(
            Egress::new(
                runtime_env.clone(),
                &NetworkPolicy::from_config(&startup_config.config["network"]),
                &home,
                "electron",
            )
            .map_err(anyhow::Error::msg)
            .context("Invalid network configuration")?,
        );
        Ok(Self {
            home,
            requested_cwd,
            cwd,
            data_dir,
            workspace,
            runtime_env,
            workspace_config,
            startup_config,
            egress,
        })
    }

    /// The Node session database and artifact root (spec rust-m11-node-storage §2.1).
    pub async fn node_database(&self) -> Result<zcode_cli_host::legacy_paths::LegacySource> {
        let config = self.workspace_config.snapshot().await;
        zcode_cli_host::legacy_paths::resolve(None, &std::env::current_dir()?, true, &config)
            .await?
            .context("Session database unavailable")
    }

    /// The Node database as the session store.
    pub async fn node_store(&self) -> Result<NodeStore> {
        let source = self.node_database().await?;
        NodeStore::open(source.database, source.artifacts).await
    }

    /// The engine over `store`: a static `--config` model or the provider
    /// registry from the environment.
    pub async fn engine(
        &self,
        store: Arc<dyn SessionStore>,
        config: Option<&PathBuf>,
        desktop: bool,
    ) -> Result<Engine> {
        let question_timing = zcode_cli_host::question_timing()?;
        let config = ModelConfig::load(config).await?;
        let registry = if config.is_none() {
            Registry::from_env(self.egress.clone())
                .await?
                .map(|r| r as Arc<dyn ModelRegistry>)
        } else {
            None
        };
        let identity = config.as_ref().map(|c| ModelIdentity {
            provider_id: c.provider_id.clone(),
            model_id: c.model_id.clone(),
            reasoning_level: c.reasoning_level.clone(),
        });
        let model = config
            .map(|c| HttpModel::new(c, self.egress.clone()))
            .map(|m| Arc::new(m) as Arc<dyn ModelPort>);
        // 与 Desktop 共用的项目 hook 信任存储：路径由用户配置文件的 storage.dir 决定。
        // 用户配置不可读时不启用信任（项目 hooks 不运行，保持 fail-closed），不影响启动。
        let user_config = std::path::Path::new(&self.startup_config.user_path);
        let trust_store =
            match zcode_cli_host::trust_store::trust_store_path(&self.home, user_config).await {
                Ok(path) => Some(Arc::new(zcode_cli_host::trust_store::FileTrustStore::new(
                    path,
                ))),
                Err(error) => {
                    tracing::warn!(
                        event = "workspace_hook.trust_store_unavailable",
                        error = %format!("{error:#}"),
                        "Workspace Hook Trust store is unavailable"
                    );
                    None
                }
            };
        let engine = Engine::new(
            self.workspace.clone(),
            identity,
            RuntimePorts {
                context: Arc::new(WorkspaceContext::new(
                    self.cwd.clone(),
                    self.home.clone(),
                    desktop,
                    self.runtime_env.vars().into(),
                )),
                store,
                model,
                tools: Arc::new(WorkspaceTools::new(
                    self.cwd.clone(),
                    self.data_dir.join("tool-results"),
                    self.workspace_config.clone(),
                    self.egress.clone(),
                )),
                clock: Arc::new(SystemClock),
            },
        )
        .await?
        .with_question_timing(question_timing.0, question_timing.1)
        .with_permission_config(&self.startup_config.config["permission"])
        .with_anomaly_guard(&self.startup_config.config["modelAnomalyGuard"])
        .with_hooks(
            &self.startup_config.config["hooks"],
            &self.startup_config.user_path,
        )
        .with_registry(registry, self.requested_cwd.to_string_lossy().into_owned());
        Ok(match trust_store {
            Some(store) => engine.with_workspace_trust(
                store,
                self.workspace_config.clone(),
                Some(env!("CARGO_PKG_VERSION").into()),
            ),
            None => engine,
        })
    }
}
