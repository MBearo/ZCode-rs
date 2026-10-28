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
use std::cell::RefCell;
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
    /// 在 panic 的线程上调用（所有捕获点都是）：panic 不再展开，同时清掉 [`note_panic`] 记下的文字。
    pub(crate) fn from_payload(payload: Box<dyn Any + Send>) -> Self {
        take_panic_note();
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

thread_local! {
    /// 本线程正在展开的 panic 的文字（TUI 的 panic hook 记下，docs/specs/rust-cli/tui.md §7.9）。
    static PANIC_NOTE: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// 在 panic hook 里记下本线程这次 panic 的文字。本线程已经记着一条（它还在展开，这次是展开中又 panic）时不记，
/// 取走并返回那一条。只用 `try_with` / `try_borrow_mut`：不等待、不 panic，线程正在销毁或借用冲突时不记。
pub fn note_panic(text: String) -> Option<String> {
    PANIC_NOTE
        .try_with(|note| {
            let mut note = note.try_borrow_mut().ok()?;
            let first = note.take();
            if first.is_none() {
                *note = Some(text);
            }
            first
        })
        .ok()
        .flatten()
}

/// 取走本线程记下的 panic 文字（栈展开到 TUI 的终端守卫时写到 stderr；被捕获时清掉）。
pub fn take_panic_note() -> Option<String> {
    PANIC_NOTE
        .try_with(|note| note.try_borrow_mut().ok()?.take())
        .ok()
        .flatten()
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "scope_tests.rs"]
mod scope_tests;

#[cfg(test)]
#[path = "owned_tests.rs"]
mod owned_tests;
