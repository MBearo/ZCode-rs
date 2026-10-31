//! 按客户端实例推来的环境变量（远端执行 F96，docs/specs/rust-cli/remote-exec.md §23.5）：`env.update` 与 SSH 代理加在
//! hello 里的 `session_env`。只在内存里（不进账本的日志，值不写日志）；之后启动的命令与 MCP stdio 服务器拿到它们
//! （`StartCommand.env`、`McpConnect.env`），已经在跑的进程不变。
//!
//! 作用范围是客户端实例：账本、流、MCP 连接本来就按它隔离；同一个实例的会话共用同一条 ssh；不同实例互不覆盖。

use std::collections::{BTreeSet, HashMap};
use std::sync::{Mutex, MutexGuard, PoisonError};

use rust_cli_exec_wire::dto::env::{EnvMode, EnvVars};
use rust_cli_kernel_api::env_command::EnvOverrides;

/// 不能改的名字：登录与 shell 的基本变量，以及 env-local 自己设的。
const PROTECTED: &[&str] = &[
    rust_cli_base::names::env::PATH,
    rust_cli_base::names::env::HOME,
    rust_cli_base::names::env::USER,
    rust_cli_base::names::env::LOGNAME,
    rust_cli_base::names::env::SHELL,
    rust_cli_base::names::env::TERM,
    rust_cli_base::names::env::PWD,
    rust_cli_base::names::env::OLDPWD,
    rust_cli_base::names::env::SHLVL,
    rust_cli_base::names::env::TMPDIR,
    "IFS",
    "ENV",
    rust_cli_base::names::env::BASH_ENV,
    "_",
    "ZCODE",
    rust_cli_base::names::env::GIT_EDITOR,
];

/// 保留的前缀：本产品的变量与改变进程加载方式的变量。
const RESERVED_PREFIXES: &[&str] = &["ZCODE_", "_ZCODE_", "LD_", "DYLD_", "BASH_FUNC_"];

/// 上限（合并之后的整组）。
const MAX_VARS: usize = 64;
const MAX_NAME: usize = 128;
const MAX_VALUE: usize = 16 * 1024;
const MAX_TOTAL: usize = 64 * 1024;

/// 一个客户端实例的那组变量与被注入的名字（以后遮蔽输出用；本线只存下来）。
#[derive(Default, Clone)]
struct ClientEnv {
    vars: EnvOverrides,
    injected: BTreeSet<String>,
}

#[derive(Default)]
pub(crate) struct EnvTable(Mutex<HashMap<String, ClientEnv>>);

fn check_name(name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    let valid = name.len() <= MAX_NAME
        && chars
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !valid {
        return Err(format!(
            "\"{name}\" is not a valid environment variable name"
        ));
    }
    if PROTECTED.contains(&name) {
        return Err(format!("\"{name}\" is protected and cannot be changed"));
    }
    if RESERVED_PREFIXES
        .iter()
        .any(|prefix| name.starts_with(prefix))
    {
        return Err(format!("\"{name}\" uses a reserved prefix"));
    }
    Ok(())
}

/// 校验整组（合并之后的）：名字、值的大小、个数与合计大小。
fn check(vars: &EnvOverrides, injected: &BTreeSet<String>) -> Result<(), String> {
    if vars.len() > MAX_VARS || injected.len() > MAX_VARS {
        return Err(format!("more than {MAX_VARS} variables"));
    }
    let mut total = 0;
    for (name, value) in vars {
        check_name(name)?;
        let size = value.as_ref().map_or(0, String::len);
        if size > MAX_VALUE {
            return Err(format!("\"{name}\" is longer than {MAX_VALUE} bytes"));
        }
        total += name.len() + size;
    }
    if total > MAX_TOTAL {
        return Err(format!("the variables are larger than {MAX_TOTAL} bytes"));
    }
    for name in injected {
        check_name(name)?;
    }
    Ok(())
}

impl EnvTable {
    fn table(&self) -> MutexGuard<'_, HashMap<String, ClientEnv>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 这个客户端实例现在的那组变量（起进程时交给 env-local）。
    pub(crate) fn overrides(&self, instance: &str) -> EnvOverrides {
        self.table()
            .get(instance)
            .map(|env| env.vars.clone())
            .unwrap_or_default()
    }

    /// 替换或合并；整组校验不通过时什么都不改，返回原因（`bad_request` 的说明）。
    pub(crate) fn update(
        &self,
        instance: &str,
        mode: EnvMode,
        vars: EnvVars,
        injected: Vec<String>,
    ) -> Result<(), String> {
        let mut table = self.table();
        let mut next = match mode {
            EnvMode::Replace => ClientEnv::default(),
            EnvMode::Merge => table.get(instance).cloned().unwrap_or_default(),
        };
        next.vars.extend(vars);
        next.injected.extend(injected);
        check(&next.vars, &next.injected)?;
        tracing::info!(
            target: "exec_server",
            client = %instance,
            names = ?next.vars.keys().collect::<Vec<_>>(),
            "environment variables updated"
        );
        table.insert(instance.to_owned(), next);
        Ok(())
    }

    /// 客户端实例被清掉（断开超过期限，§19.7）时一起忘掉。
    pub(crate) fn forget(&self, instance: &str) {
        self.table().remove(instance);
    }
}
