// 复制自 rust/crates/async/src/lib.rs 与 rust/crates/kit/src/lib.rs（feat/rs 79b6691c55）。
//! 基础层：任务监督、秘密值、脱敏、JavaScript 语义的文本工具（docs/specs/rust-cli/README.md §3）。
//!
//! - [`TaskScope`] / [`owned`] / [`catch_panic`]：任务归属与 panic 转错误（原 zcode-async）。
//! - [`secret::RedactedString`]、[`redact::redact_secrets`]：秘密值与按值脱敏（原 zcode-kit）。
//! - [`js`]：与 JavaScript 语义一致的字符串、`JSON.stringify`、`toISOString`（原 zcode-kit）。
//! - [`names`]：产品目录与文件的名字（`.zcode`、`AGENTS.md` 等），同一个名字只定义一处（F55）。
//! - [`random`]：随机数的入口（`RandomState` 的种子，F58）。
//! - [`glob`]、[`path`]、[`reminder`]、[`log_text`]：几个 crate 共用的小函数，只定义一处（F58，README §4.1）。
//!
//! 异步任务（docs/specs/rust-u0-foundation.md §3.3）：
//!
//! tokio 会在任务边界上捕获 panic，只结束出错的任务；但 spawn 出去、负责发送终态或延迟回复的
//! 任务一旦 panic，等待方就永远收不到结果。[`catch_panic`] 把 panic 变成普通的错误值，
//! 让这类任务在任何情况下都能走到发送终态的那一步。
//!
//! [`TaskScope`] 给任务一个归属（docs/specs/rust-u2-task-scope.md）：所有者释放作用域时任务被
//! 中止，panic 被上报，关闭时能列出还在运行的任务。在同一个函数里启动并等待的结构化任务由
//! 句柄拥有（[`owned`]），阻塞调用用 [`blocking`]。

use std::any::Any;
use std::fmt;
use std::future::Future;
use std::panic::AssertUnwindSafe;

use futures_util::FutureExt;

pub mod embed;
pub mod glob;
pub mod js;
pub mod log_text;
pub mod names;
mod owned;
pub mod path;
pub mod random;
pub mod redact;
pub mod reminder;
pub mod rule_url;
mod scope;
pub mod secret;

pub use owned::{OwnedTask, blocking, owned};
pub use scope::{Spawner, Task, TaskError, TaskPanic, TaskScope};

/// 运行 `future`；它 panic 时返回 [`Panicked`]，不再向外传播。
///
/// 只用于“无论成功、失败还是 panic 都必须发出终态”的任务。panic 之后，`future` 持有的状态
/// 可能处于中间状态，调用方只能把它当作失败处理，不能继续使用其中的数据。
pub async fn catch_panic<F: Future>(future: F) -> Result<F::Output, Panicked> {
    AssertUnwindSafe(future)
        .catch_unwind()
        .await
        .map_err(Panicked::from_payload)
}

/// 被捕获的 panic；`message` 是 panic 时给出的字符串（非字符串载荷时为固定说明）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Panicked {
    pub message: String,
}

impl Panicked {
    pub(crate) fn from_payload(payload: Box<dyn Any + Send>) -> Self {
        let message = payload
            .downcast_ref::<&str>()
            .map(|text| (*text).to_owned())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "non-string panic payload".to_owned());
        Self { message }
    }
}

impl fmt::Display for Panicked {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "task panicked: {}", self.message)
    }
}

impl std::error::Error for Panicked {}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "scope_tests.rs"]
mod scope_tests;

#[cfg(test)]
#[path = "owned_tests.rs"]
mod owned_tests;
