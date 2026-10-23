//! lib.rs 用的小工具（从 lib.rs 拆出，保持单文件不超过 400 行）。

use std::path::{Path, PathBuf};

use rust_cli_kernel_api::env::{EnvError, EnvPath};
use rust_cli_kernel_api::ids::OperationId;
use tokio_util::sync::CancellationToken;

pub(crate) fn unknown_operation(operation: &OperationId) -> EnvError {
    EnvError::NotFound {
        path: format!("operation {operation}"),
    }
}

pub(crate) fn unsupported(what: &str) -> EnvError {
    EnvError::Unsupported {
        what: format!("local environment {what}"),
    }
}

pub(crate) fn path(value: &EnvPath) -> PathBuf {
    PathBuf::from(value.as_str())
}

/// 在阻塞线程上执行一次文件 IO（仓库规则：异步代码里不做同步 IO）。
pub(crate) async fn run<T, F>(work: F) -> Result<T, EnvError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, EnvError> + Send + 'static,
{
    rust_cli_base::blocking(work)
        .await
        .map_err(|error| EnvError::Io {
            message: error.to_string(),
        })?
}

/// 同 [`run`]，给可以中途停下的遍历用（R5-13）：调用方丢下返回的 future 时（工具被取消、界面退出），
/// `work` 收到的令牌被取消，遍历在下一个条目处停下。原来阻塞线程里的遍历只能跑完整棵树（丢下 future 只是
/// 不再等它），esc 之后回合要等它，进程退出也要等它。
pub(crate) async fn run_walk<T, F>(work: F) -> Result<T, EnvError>
where
    T: Send + 'static,
    F: FnOnce(&CancellationToken) -> Result<T, EnvError> + Send + 'static,
{
    let stop = CancellationToken::new();
    let _cancel_on_drop = stop.clone().drop_guard();
    run(move || work(&stop)).await
}

pub(crate) fn on_path<T, F>(
    value: &EnvPath,
    work: F,
) -> impl FnOnce() -> Result<T, EnvError> + use<T, F>
where
    F: FnOnce(&Path) -> Result<T, EnvError>,
{
    let target = path(value);
    move || work(&target)
}
