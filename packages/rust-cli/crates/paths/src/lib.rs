//! 本机存储位置的唯一解析（docs/specs/rust-cli/README.md §4）。
//!
//! db、artifacts 等本机存储的位置只在这里决定；以后改存储路径只改这一处。工作区里的文件不是
//! 本机存储，它们经执行端口（kernel-api `machine`）访问，类型是 `EnvPath`，不能用 [`StoragePath`] 表示。
//!
//! 规则与 Node CLI 相同（docs/specs/rust-m11-node-storage.md §2.1）：
//! - 会话库默认 `~/.zcode/cli/db/db.sqlite`，`storage.sessionDbPath` 覆盖；
//! - 产物根目录是 `<storage.dir>/cli/artifacts`，`storage.dir` 默认 `~/.zcode`；
//! - 只展开 `~` 与 `~/…`，其余相对路径相对调用方给的基准目录；
//! - 自动记忆目录：`<storage.dir>/projects/<sanitizePath(项目)>/memory`
//!   （根换成 zcode 的存储根，docs/specs/rust-cli/context.md §2）。
//!
//! 配置分层与 `ZCODE_*` 环境变量覆盖由配置层先算好，这里只接收结果。
//!
//! 账号与 Provider 的配置（docs/specs/rust-cli/platform.md §2）与 Node CLI、App 共用：
//! - 配置根 `<dataBaseDir>/.zcode/v2`，`dataBaseDir` 默认 home，入口读 `ZCODE_DATA_BASE_DIR` 后经
//!   [`StoragePaths::with_data_base_dir`] 覆盖（Node `resolveSharedZCodeCredentialsPath`）；
//! - 其下 `provider_config.json`（个人 Provider 配置）、`credentials.json`（加密凭据）、
//!   `runtime/provider/`（内置 Provider 配置的缓存）、`telemetry-state.json`（设备身份 `deviceMid`，F52）；
//! - 设置文件固定在 `<home>/.zcode/v2/setting.json`（Host `SettingStore`），不跟随 `dataBaseDir`。
//!
//! Node CLI 的用户级配置与项目 hooks 的信任记录（docs/specs/rust-cli/config.md §1、§4.2）：
//! - 用户级配置固定在 `<home>/.zcode/cli/config.json`，不跟随 `storage.dir`；
//! - 信任记录在 `<存储根>/security/workspace-hook-trust-v1.json`，存储根是用户级配置的 `storage.dir`
//!   （相对路径相对 home，不随 cwd 漂移），没有时 `<home>/.zcode`。
//!
//! Node 的插件存储（docs/specs/rust-cli/ext.md §4.1）：`<storage.dir>/cli/plugins`（`installed_plugins.json`、
//! `cache/`、`marketplaces/`）；`storage.dir` 是合并后的值，相对路径相对调用方给的基准目录。

mod home;

use std::path::{Path, PathBuf};

pub use home::{expand_home, home_dir_with, user_name_with};

/// 产品目录名（`.zcode`）。名字定义在 base（内核 crate 也要用，它们不能依赖 paths，README §3.1）；用户根的位置
/// 只在 [`user_path`] 计算。
pub use rust_cli_base::names::DOT_DIR;

/// Node CLI 的存储子目录 `cli`（Node `getCliStorageRoot`）；下面的位置都从它拼（`concat!` 里不能用 `const`）。
macro_rules! cli_dir {
    () => {
        "cli"
    };
}
/// 本工程专用的子目录 `rust-cli`（日志、跨会话登记、自动更新的缓存）。
macro_rules! rust_cli_dir {
    () => {
        "rust-cli"
    };
}

/// 用户根下的会话库（Node `getSessionDbPath` 的缺省值，相对用户根）。公开给示例程序：它们在临时的存储根下按同样的
/// 布局放会话库（F58：以前 node-store 与 cli 的示例各写一次 `cli/db/db.sqlite`）。
pub const DEFAULT_SESSION_DB: &str = concat!(cli_dir!(), "/db/db.sqlite");
const ARTIFACTS_DIR: &str = concat!(cli_dir!(), "/artifacts");
const PROJECTS_DIR: &str = "projects";
const MEMORY_DIR: &str = "memory";
/// 任务清单（Task v2）目录（docs/specs/rust-cli/agents.md §7.2）。
const TASKS_DIR: &str = "tasks";
/// 本工程的运行日志目录（docs/specs/rust-cli/logging.md §1；Node CLI 的在 `cli/log`）。
const LOG_DIR: &str = concat!(rust_cli_dir!(), "/log");
/// 跨会话的登记处与收件箱 socket（docs/specs/rust-cli/cross-session.md §2.2）。
const PEERS_DIR: &str = concat!(rust_cli_dir!(), "/peers");
/// 自动更新的缓存（docs/specs/rust-cli/update.md §4，F48；Codex `version.json`）。
const UPDATE_CACHE: &str = concat!(rust_cli_dir!(), "/version.json");
/// 远端机器的清单（docs/specs/rust-cli/remote-exec.md §20.3.1）。
const MACHINES_FILE: &str = concat!(rust_cli_dir!(), "/machines.json");
/// MCP 服务器绑定到哪台机器（docs/specs/rust-cli/remote-exec.md §22.2）。
const MCP_MACHINES_FILE: &str = concat!(rust_cli_dir!(), "/mcp-machines.json");
/// 规整后目录名的上限（UTF-16 码元）。
const MAX_SANITIZED_LENGTH: usize = 200;
/// `<dataBaseDir>` 的用户根下的配置根（`.zcode/v2`）。
const CONFIG_ROOT: &str = "v2";
/// 用户根下 Node CLI 的用户级配置（`DEFAULT_BASE_DIR` + `DEFAULT_CONFIG_FILE`）。
const CLI_CONFIG: &str = concat!(cli_dir!(), "/config.json");
/// 项目 hooks 的信任记录（Node `SECURITY_DIRECTORY` / `TRUST_STORE_FILE`）。
const HOOK_TRUST_STORE: &str = "security/workspace-hook-trust-v1.json";
/// Node `getCliStorageRoot` 的子目录与 `getPluginStorageRoot` 的子目录。
const CLI_DIR: &str = cli_dir!();
const PLUGINS_DIR: &str = "plugins";
/// 模型轨迹的目录（Node `getModelIoDir`：开发态 `debug`，其余 `rollout`，docs/specs/rust-cli/model-io.md §3.2）。
const MODEL_IO_DEBUG_DIR: &str = "debug";
const MODEL_IO_ROLLOUT_DIR: &str = "rollout";

/// 用户根（`<home>/.zcode`）下的位置；`rest` 为空时是用户根本身（Node `DEFAULT_BASE_DIR`、Host 的 `~/.zcode`）。
///
/// 本机存储里用户根只在这里计算：会话库与存储根的缺省值、配置根 `.zcode/v2`（`home` 换成 `ZCODE_DATA_BASE_DIR`
/// 时也是）、用户级配置、信任记录的缺省根、TUI `/memory` 的用户规则文件都由它推出（F55）。以后要让用户根可配置，
/// 只改这里，缺省值不变。
///
/// 拼法保持收拢前的样子：`rest`（用 `/` 写）整段接在 `<home>/` 后面，即 `home.join(".zcode/<rest>")`；所以 Windows
/// 上是 `C:\Users\x\.zcode/cli/db/db.sqlite` 这样的混用分隔符（另案处理，docs/specs/rust-cli/config.md §2.3）。
pub fn user_path(home: &Path, rest: &str) -> PathBuf {
    if rest.is_empty() {
        home.join(DOT_DIR)
    } else {
        home.join(format!("{DOT_DIR}/{rest}"))
    }
}

/// 一个本机存储位置。只能从 [`StoragePaths`] 得到，工作区路径不能冒充它。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoragePath(PathBuf);

impl StoragePath {
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// 存储位置下的子路径，例如某个会话的产物目录。
    pub fn join(&self, child: impl AsRef<Path>) -> Self {
        Self(self.0.join(child))
    }
}

/// 解析存储位置所需的配置值（已经过配置分层与环境变量覆盖）。
#[derive(Debug, Clone, Copy, Default)]
pub struct StorageSettings<'a> {
    /// `storage.sessionDbPath`。
    pub session_db_path: Option<&'a str>,
    /// `storage.dir`。
    pub storage_dir: Option<&'a str>,
}

/// 本机存储位置的集合。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoragePaths {
    database: StoragePath,
    artifacts: StoragePath,
    root: StoragePath,
    config: StoragePath,
    settings: StoragePath,
    builtin_provider_config: Option<StoragePath>,
    home: PathBuf,
}

impl StoragePaths {
    /// `home` 用于展开 `~`；`base` 是相对路径的基准目录（由入口 crate 给出）。
    pub fn resolve(settings: &StorageSettings<'_>, home: &Path, base: &Path) -> Self {
        // 缺省值是用户根下的位置（收拢前写成 `~/.zcode/…` 再展开，结果相同）；配置的值展开 `~`、相对 `base`。
        let resolve = |value: Option<&str>, default: &str| match value {
            Some(value) => base.join(expand_home(value, home)),
            None => base.join(user_path(home, default)),
        };
        let root = resolve(settings.storage_dir, "");
        Self {
            database: StoragePath(resolve(settings.session_db_path, DEFAULT_SESSION_DB)),
            artifacts: StoragePath(root.join(ARTIFACTS_DIR)),
            root: StoragePath(root),
            config: StoragePath(user_path(home, CONFIG_ROOT)),
            settings: StoragePath(user_path(home, CONFIG_ROOT).join("setting.json")),
            builtin_provider_config: None,
            home: home.to_path_buf(),
        }
    }

    /// 显式给出的内置 Provider 配置文件（`ZCODE_BUILTIN_PROVIDER_CONFIG_FILE`，绝对路径）；没有时用随包配置。
    pub fn with_builtin_provider_config(mut self, file: &Path) -> Self {
        self.builtin_provider_config = Some(StoragePath(file.to_path_buf()));
        self
    }

    /// 显式给出的内置 Provider 配置文件。
    pub fn builtin_provider_config(&self) -> Option<&StoragePath> {
        self.builtin_provider_config.as_ref()
    }

    /// 账号与 Provider 配置的根改到 `<base>/.zcode/v2`（`ZCODE_DATA_BASE_DIR`）；`base` 是绝对路径。
    pub fn with_data_base_dir(mut self, base: &Path) -> Self {
        self.config = StoragePath(user_path(base, CONFIG_ROOT));
        self
    }

    /// 账号与 Provider 配置的根（默认 `~/.zcode/v2`）。
    pub fn zcode_config(&self) -> &StoragePath {
        &self.config
    }

    /// 个人 Provider 配置 `provider_config.json`（与 Node、App 共用）。
    pub fn provider_config(&self) -> StoragePath {
        self.config.join("provider_config.json")
    }

    /// 加密凭据 `credentials.json`（与 Node、App 共用）。
    pub fn credentials(&self) -> StoragePath {
        self.config.join("credentials.json")
    }

    /// 设备身份所在的 `telemetry-state.json`（字段 `deviceMid`，与 App、Node CLI 共用；工作线 F52，
    /// docs/specs/rust-cli/platform.md §10）。
    pub fn telemetry_state(&self) -> StoragePath {
        self.config.join("telemetry-state.json")
    }

    /// 读写 `telemetry-state.json` 时的锁文件 `telemetry-state.lock`（Node CLI `withTelemetryStateLock`）。
    pub fn telemetry_lock(&self) -> StoragePath {
        self.config.join("telemetry-state.lock")
    }

    /// 内置 Provider 配置的缓存目录 `runtime/provider/`。
    pub fn builtin_provider_cache(&self) -> StoragePath {
        self.config.join("runtime").join("provider")
    }

    /// App 的设置文件 `~/.zcode/v2/setting.json`。
    /// TUI 的输入历史（与 Host TUI 相同：`<dataBaseDir>/.zcode/v2/tui/history.jsonl`）。
    pub fn tui_history(&self) -> StoragePath {
        self.config.join("tui").join("history.jsonl")
    }

    pub fn settings(&self) -> &StoragePath {
        &self.settings
    }

    /// Node 的会话库 `db.sqlite`。
    pub fn database(&self) -> &StoragePath {
        &self.database
    }

    /// 产物根目录；每个会话一个子目录（`zcode-artifact://<sessionId>/<artifactId>`）。
    pub fn artifacts(&self) -> &StoragePath {
        &self.artifacts
    }

    /// 自动记忆目录。`project` 是项目的规范路径（git 根，不在仓库里时用项目根），
    /// 由调用方从执行环境的事实算好。
    pub fn memory_dir(&self, project: &str) -> StoragePath {
        self.root
            .join(PROJECTS_DIR)
            .join(sanitize_path(project))
            .join(MEMORY_DIR)
    }
}

impl StoragePaths {
    /// Node CLI 的用户级配置 `<home>/.zcode/cli/config.json`（与 Node、App 共用）。
    pub fn cli_config(&self) -> StoragePath {
        StoragePath(user_path(&self.home, CLI_CONFIG))
    }

    /// 项目 hooks 的信任记录。`storage_dir` 是用户级配置的 `storage.dir`（Node
    /// `resolveWorkspaceHookTrustStorePath` / `resolveTrustedUserPath`：`~/` 展开到 home，相对路径相对
    /// home，空白视为没有）。
    pub fn hook_trust_store(&self, storage_dir: Option<&str>) -> StoragePath {
        let root = match storage_dir.map(str::trim).filter(|dir| !dir.is_empty()) {
            Some(dir) => self.home.join(expand_home(dir, &self.home)),
            None => user_path(&self.home, ""),
        };
        StoragePath(root.join(HOOK_TRUST_STORE))
    }

    /// Node 的插件存储根（`getPluginStorageRoot(getCliStorageRoot(resolvePath(storage.dir)))`）：
    /// `storage_dir` 是合并后的 `storage.dir`（空白视为没有，缺省 `~/.zcode`），`~/` 展开到 home，其余相对
    /// 路径相对 `base`（Node `resolve` 相对进程 cwd）；末段已是 `cli` 时不再加一层。
    pub fn plugin_storage(&self, storage_dir: Option<&str>, base: &Path) -> StoragePath {
        StoragePath(self.cli_storage_root(storage_dir, base).join(PLUGINS_DIR))
    }

    /// 模型轨迹的目录（工作线 F91，docs/specs/rust-cli/model-io.md §3.2）：Node 的 `getModelIoDir(getCliStorageRoot(
    /// resolvePath(storage.dir)), isDevelopment)`，cli 存储根的规则同 [`Self::plugin_storage`]；开发态 `debug`，其余
    /// `rollout`。与 Node 共用这个目录。
    pub fn model_io(
        &self,
        storage_dir: Option<&str>,
        base: &Path,
        development: bool,
    ) -> StoragePath {
        let dir = if development {
            MODEL_IO_DEBUG_DIR
        } else {
            MODEL_IO_ROLLOUT_DIR
        };
        StoragePath(self.cli_storage_root(storage_dir, base).join(dir))
    }

    /// Node `getCliStorageRoot(resolvePath(storage.dir))`：`storage_dir` 空白视为没有（缺省 `~/.zcode`），`~/` 展开到
    /// home，其余相对路径相对 `base`；末段已是 `cli` 时不再加一层。
    fn cli_storage_root(&self, storage_dir: Option<&str>, base: &Path) -> PathBuf {
        let root = match storage_dir.map(str::trim).filter(|dir| !dir.is_empty()) {
            Some(dir) => base.join(expand_home(dir, &self.home)),
            None => base.join(user_path(&self.home, "")),
        };
        if root.file_name().is_some_and(|name| name == CLI_DIR) {
            root
        } else {
            root.join(CLI_DIR)
        }
    }

    /// 顶层会话的任务清单文件 `<storage.dir>/tasks/<session>.json`（一份清单一个文件）。`session` 是会话 ID，
    /// 只含字母、数字、`_` 与 `-`。
    pub fn task_list(&self, session: &str) -> StoragePath {
        self.root.join(TASKS_DIR).join(format!("{session}.json"))
    }

    /// 运行日志目录 `<storage.dir>/rust-cli/log`（docs/specs/rust-cli/logging.md §1）。
    pub fn logs(&self) -> StoragePath {
        self.root.join(LOG_DIR)
    }

    /// 跨会话的登记目录 `<storage.dir>/rust-cli/peers`：每个进程一份 `<pid>.json` 与它的 `<pid>.sock`
    /// （docs/specs/rust-cli/cross-session.md §2.2，F28）。
    pub fn peers(&self) -> StoragePath {
        self.root.join(PEERS_DIR)
    }

    /// 执行器的目录 `~/.zcode/rust-cli/exec/`（docs/specs/rust-cli/remote-exec.md §19.4）：在用户根下，不跟
    /// `storage.dir` 走（执行器模式不读配置；远端也按同一个位置找）。
    pub fn executor(&self) -> ExecutorPaths {
        ExecutorPaths {
            dir: StoragePath(user_path(&self.home, rust_cli_base::names::EXECUTOR_HOME)),
        }
    }

    /// 自动更新的缓存 `<storage.dir>/rust-cli/version.json`：最近一次核对的最新版本、核对时刻与不再提醒的版本
    /// （docs/specs/rust-cli/update.md §4，F48）。
    pub fn update_cache(&self) -> StoragePath {
        self.root.join(UPDATE_CACHE)
    }

    /// 远端机器的清单 `~/.zcode/rust-cli/machines.json`（docs/specs/rust-cli/remote-exec.md §20.3.1）：用户根下，不随
    /// `storage.dir`（机器是用户级的配置，不是会话数据）。
    pub fn machines(&self) -> StoragePath {
        StoragePath(user_path(&self.home, MACHINES_FILE))
    }

    /// MCP 服务器绑定到远端机器的旁路文件 `~/.zcode/rust-cli/mcp-machines.json`（docs/specs/rust-cli/remote-exec.md
    /// §22.2）：用户根下，不随 `storage.dir`（同 [`StoragePaths::machines`]）。
    pub fn mcp_machines(&self) -> StoragePath {
        StoragePath(user_path(&self.home, MCP_MACHINES_FILE))
    }
}

/// 执行器目录下的位置（docs/specs/rust-cli/remote-exec.md §19.4）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutorPaths {
    dir: StoragePath,
}

impl ExecutorPaths {
    /// 目录本身（0700）。
    pub fn dir(&self) -> &StoragePath {
        &self.dir
    }

    /// 常驻执行器的 socket。
    pub fn socket(&self) -> StoragePath {
        self.dir.join("executor.sock")
    }

    /// 单实例锁（操作系统的文件锁）。
    pub fn lock(&self) -> StoragePath {
        self.dir.join("executor.lock")
    }

    /// 代理之间的拉起锁。
    pub fn spawn_lock(&self) -> StoragePath {
        self.dir.join("spawn.lock")
    }

    /// 文件版账本的目录。
    pub fn ledger(&self) -> StoragePath {
        self.dir.join("ledger")
    }

    /// 常驻执行器与代理的运行日志目录。
    pub fn logs(&self) -> StoragePath {
        self.dir.join("log")
    }

    /// 上传的执行器 `bin/<版本>/zcode-rs`。
    pub fn binary(&self, version: &str) -> StoragePath {
        self.dir
            .join("bin")
            .join(version)
            .join(rust_cli_base::names::UNIX_EXECUTABLE)
    }

    /// 本机侧缓存的 Linux 产物 `dist/<版本>/<target>/zcode-rs`（上传用）。
    pub fn artifact(&self, version: &str, target: &str) -> StoragePath {
        self.dir
            .join("dist")
            .join(version)
            .join(target)
            .join(rust_cli_base::names::UNIX_EXECUTABLE)
    }
}

/// 把路径变成一个目录名（`sanitizePath`）：每个不是 ASCII 字母数字的 UTF-16 码元换成
/// `-`；超过 200 个码元时截断并加 djb2 哈希（Node 的回退实现；用 Bun.hash 的实现
/// 超长路径的后缀与它不同）。
pub fn sanitize_path(name: &str) -> String {
    let units: Vec<u16> = name.encode_utf16().collect();
    let sanitized: String = units
        .iter()
        .map(|&unit| match u8::try_from(unit) {
            Ok(byte) if byte.is_ascii_alphanumeric() => char::from(byte),
            _ => '-',
        })
        .collect();
    if units.len() <= MAX_SANITIZED_LENGTH {
        return sanitized;
    }
    let hash = units.iter().fold(0i32, |hash, &unit| {
        (hash << 5).wrapping_sub(hash).wrapping_add(i32::from(unit))
    });
    format!(
        "{}-{}",
        &sanitized[..MAX_SANITIZED_LENGTH],
        rust_cli_base::js::base36(u64::from(hash.unsigned_abs()))
    )
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "user_root_tests.rs"]
mod user_root_tests;
