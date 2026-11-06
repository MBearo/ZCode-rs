//! 启动时从进程捕获的事实，按角色分开（F129，审查 E4；docs/specs/rust-cli/cli-args.md §14）：
//!
//! | 结构 | 内容 | 谁用 |
//! | --- | --- | --- |
//! | [`ProcessFacts`] | home、cwd、环境快照、临时目录、时区 | 每个角色（执行器只拿它） |
//! | [`ControllerFacts`] | 账号与 Provider 配置的位置、凭据的密钥、显式覆盖的端点与模型、输出上限、用户名、运行的平台 | controller（TUI、`-p`、app-server、账号与信任命令） |
//! | [`TestHooks`] | 只给测试的 12 个 `_ZCODE_ASSUME_*`（只在 debug 构建读） | controller 的组装 |
//!
//! 以前三样放在一个 `Startup` 里，执行器也 `capture()`，于是持有 API key 与凭据密钥（与 deployment.md §3.5“执行器不读
//! 模型凭据”不一致）；测试声明由 5 个函数各读一部分；`startup.explicit` 被移进模型的组装之后就不能再读测试声明，组装的
//! 顺序因此受限。现在执行器只捕获 [`ProcessFacts`]，测试声明在 [`TestHooks::read`] 里读一次。

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use rust_cli_base::names::env;
use rust_cli_base::secret::{EnvSnapshot, RedactedString};
use rust_cli_paths::{
    ExecutorPaths, StoragePaths, StorageSettings, expand_home, home_dir_with, user_name_with,
};

use crate::assemble::options::{Assumptions, MachineSsh, SshAssumption, TestDeclarations};
use crate::{
    assemble::limits, assemble::models, assemble::setup, process::os_release, process::temp_dir,
    process::time_zone,
};

/// 每个角色都要的进程事实。
pub(crate) struct ProcessFacts {
    /// Node `os.homedir()`（paths `home_dir_with`，config.md §2.3）：本机存储、`~` 展开、凭据密钥与执行环境的 home
    /// 都用它。F57 之前这里在所有平台先看 `HOME`，凭据密钥另取一份（Windows 上 `USERPROFILE`），Windows 上两者不同。
    pub(crate) home: PathBuf,
    pub(crate) cwd: PathBuf,
    /// 进程环境快照（用户命令按白名单投影，shell-permissions.md §2.3；`Debug` 只列变量名）。
    pub(crate) env: EnvSnapshot,
    pub(crate) temp: PathBuf,
    /// 系统时区（time-zone.md §1）：启动时读一次。
    pub(crate) zone: Arc<dyn rust_cli_base::clock::TimeZone>,
    /// 进程启动的时刻（单调时钟）：启动阶段日志的起点（`startup.ready`、`startup.model`，logging.md §4）。
    pub(crate) started: std::time::Instant,
}

impl ProcessFacts {
    #[expect(
        clippy::disallowed_methods,
        reason = "入口 crate 是唯一读取 cwd 的位置（spec §4）"
    )]
    pub(crate) fn capture() -> Result<Self> {
        let started = std::time::Instant::now();
        // 修复线 F58：以前除了快照，这里又按名字直接读进程环境（约 13 个变量，`std::env::var` 在 Windows 上不分大小写，
        // 快照按原样比较）。现在先捕获快照，所有变量都从它取，名字的比较只有 `EnvSnapshot::get` 一种（Windows 上不分
        // 大小写，照 Node）。值是快照里的 UTF-8 文本（非 UTF-8 的字节换成 U+FFFD，Node 的 `process.env` 相同）。
        let env: EnvSnapshot = setup::process_env().into();
        let os = |name: &str| env.get(name).map(std::ffi::OsString::from);
        let home = home_dir_with(os, setup::windows_profile)?;
        let cwd = std::env::current_dir().context("reading the current directory")?;
        // 修复（R2-6）：以前只取 `TZ` 的名字，时钟一律按 UTC；现在读系统时区，时钟按它给本地日期与偏移。
        let zone = time_zone::system(env.get(env::TZ));
        Ok(Self {
            temp: temp_dir::node_tmpdir(&env, cfg!(windows)),
            home,
            cwd,
            env,
            zone,
            started,
        })
    }

    /// 本机执行器的目录（`~/.zcode/rust-cli/exec/`，remote-exec.md §19.4）：在用户根下，不随 `storage.dir`。执行器自己、
    /// 上传用的产物缓存与执行器的 npm 来源共用这一个位置（F129：以前三处各算一遍）。
    pub(crate) fn executor_paths(&self) -> ExecutorPaths {
        self.default_paths().executor()
    }

    /// 读配置之前的存储位置（用户根下的缺省；运行日志、执行器）。不看账号与 Provider 配置的位置（那是 controller 的事实，
    /// [`ControllerFacts::storage_paths`]）。
    pub(crate) fn default_paths(&self) -> StoragePaths {
        StoragePaths::resolve(&StorageSettings::default(), &self.home, &self.cwd)
    }
}

/// 只有 controller 读的事实：执行器不构造凭据、会话库与模型客户端（deployment.md §3.5）。
pub(crate) struct ControllerFacts {
    /// 凭据密钥要的用户名（Node `os.userInfo().username`）。
    pub(crate) user_name: String,
    /// `ZCODE_DATA_BASE_DIR`：账号与 Provider 配置的根（Node 相同）。
    pub(crate) data_base_dir: Option<String>,
    /// `ZCODE_BUILTIN_PROVIDER_CONFIG_FILE`：显式的内置 Provider 配置。
    pub(crate) builtin_provider_config: Option<String>,
    /// `ZCODE_CREDENTIAL_SECRET`。
    pub(crate) credential_secret: Option<RedactedString>,
    /// 显式覆盖的端点、key 与模型（`ZCODE_*`，model.md §6）。
    pub(crate) explicit: models::Override,
    /// `ZCODE_MAX_OUTPUT_TOKENS` 的有效值：入口读一次，模型端口、平台层、压缩与内核的缺省共用（F58）。
    pub(crate) max_output_tokens: Option<u32>,
    /// 运行的平台与 Windows 的系统版本（TUI 的按键规则与颜色级别，修复线 F65）。
    pub(crate) os: rust_cli_tui::Os,
}

impl ControllerFacts {
    pub(crate) fn read(process: &ProcessFacts) -> Self {
        let env = &process.env;
        let os = |name: &str| env.get(name).map(std::ffi::OsString::from);
        Self {
            user_name: user_name_with(os),
            data_base_dir: env.text(env::ZCODE_DATA_BASE_DIR).map(str::to_owned),
            builtin_provider_config: env
                .text(env::ZCODE_BUILTIN_PROVIDER_CONFIG_FILE)
                .map(str::to_owned),
            credential_secret: env
                .text(env::ZCODE_CREDENTIAL_SECRET)
                .map(RedactedString::new),
            // 显式覆盖的名字（修复线 F42，config.md §2.5）：只读 `ZCODE_*`；端点不叫 `ZCODE_BASE_URL`（它是 Node 的 ZCode 服务
            // 地址）。
            explicit: models::Override {
                base_url: env.get(env::ZCODE_MODEL_BASE_URL).map(str::to_owned),
                api_key: env.get(env::ZCODE_API_KEY).map(RedactedString::new),
                model: env.text(env::ZCODE_MODEL).map(str::to_owned),
            },
            max_output_tokens: limits::max_output_tokens(env),
            os: os_release::current(),
        }
    }

    /// 存储位置：环境变量给的路径展开 `~`，相对路径相对 cwd（Node `resolveUserPath`）。`storage` 是配置层与环境变量合并后的
    /// `storage.*`（会话库与存储根，修复线 F2）；读配置之前（运行日志、平台层）用缺省。
    pub(crate) fn storage_paths(
        &self,
        process: &ProcessFacts,
        storage: &StorageSettings<'_>,
    ) -> StoragePaths {
        let resolve = |value: &str| process.cwd.join(expand_home(value, &process.home));
        let mut paths = StoragePaths::resolve(storage, &process.home, &process.cwd);
        if let Some(base) = &self.data_base_dir {
            paths = paths.with_data_base_dir(&resolve(base));
        }
        if let Some(file) = &self.builtin_provider_config {
            paths = paths.with_builtin_provider_config(&resolve(file));
        }
        paths
    }
}

/// 只给测试的声明（`_ZCODE_ASSUME_*`，model.md §6、remote-exec.md §18.6–§20.10、provider-errors.md D13）：只在 debug
/// 构建读，启动时读一次。不是给用户的开关。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TestHooks {
    /// 布尔的声明与按哪个模型查规则。
    pub(crate) assume: Assumptions,
    /// SSH 的测试入口（主 env 是那台机器）。
    pub(crate) ssh: Option<SshAssumption>,
    /// 查请求方案与错误码表时把端点当成这个地址。
    pub(crate) assumed_base_url: Option<String>,
    /// 机器清单里的机器经 SSH 时的配置文件、产物与执行器的假注册表。
    pub(crate) machine_ssh: MachineSsh,
}

impl TestHooks {
    /// 读全部声明；`Ignored`（release 构建）时都是缺省。
    pub(crate) fn read(declarations: TestDeclarations, env: &EnvSnapshot) -> Self {
        Self {
            assume: crate::assemble::options::assumptions(declarations, env),
            ssh: crate::assemble::options::ssh_assumption(declarations, env),
            assumed_base_url: crate::assemble::options::assumed_base_url(declarations, env),
            machine_ssh: crate::assemble::options::machine_ssh(declarations, env),
        }
    }
}

/// controller 的全部启动事实。
pub(crate) struct Facts {
    pub(crate) process: ProcessFacts,
    pub(crate) controller: ControllerFacts,
    pub(crate) hooks: TestHooks,
}

impl Facts {
    pub(crate) fn capture() -> Result<Self> {
        let process = ProcessFacts::capture()?;
        let controller = ControllerFacts::read(&process);
        let hooks = TestHooks::read(TestDeclarations::BUILD, &process.env);
        Ok(Self {
            process,
            controller,
            hooks,
        })
    }

    /// [`ControllerFacts::storage_paths`]。
    pub(crate) fn storage_paths(&self, storage: &StorageSettings<'_>) -> StoragePaths {
        self.controller.storage_paths(&self.process, storage)
    }
}
